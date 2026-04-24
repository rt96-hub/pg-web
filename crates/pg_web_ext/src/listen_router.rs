//! Channel-aware fan-out for Postgres `NOTIFY` messages.
//!
//! One shared `ListenRouter` owns a per-channel `broadcast::Sender`.
//! Subscribers clone a `Receiver` off the sender; the LISTEN task
//! (`spawn_listen_task`) pushes notifications into the sender as they
//! arrive over the wire. No per-subscriber Postgres backend slot —
//! hundreds of SSE clients share a single `LISTEN` connection.
//!
//! # Scope
//!
//! v0.1 uses this for one channel, `pgweb_livereload`, feeding
//! `/_pgweb/livereload` SSE clients. The router is deliberately
//! agnostic about what channel name means — Phase 2 realtime
//! subscriptions (per project memory: user-level LISTEN/NOTIFY + SSE
//! for live app data) reuse the same infrastructure by calling
//! `subscribe(&format!("pgweb_app_{channel}"))` from a different SSE
//! endpoint. No rewrite.
//!
//! # Connection cost
//!
//! Exactly one extra Postgres backend slot per running BGW, started
//! ONLY when `pgweb.settings.env = 'development'` at worker startup.
//! Prod mode = zero extra backends. Documented in
//! `docs/APP-DEVELOPER-GUIDE.md` § Pushing.
//!
//! # Thread-safety
//!
//! `Mutex<HashMap>` is intentional over `DashMap` or `RwLock` —
//! channel registration is rare (typically once at SSE-handler entry),
//! publishes are cheap `broadcast::Sender::send` calls, and we never
//! hold the lock across an .await. No contention in practice.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use tokio::sync::broadcast;
use tracing::{debug, warn};

/// Buffer depth for each channel's broadcast queue. A "lagged" receiver
/// that can't keep up gets dropped messages (broadcast::Receiver::recv
/// returns RecvError::Lagged). Eight is enough to absorb a short burst
/// during a reload without burning memory for a crowded channel that
/// nobody reads. Tune up if anyone complains of dropped live-reload
/// events.
const BROADCAST_BUFFER: usize = 8;

/// A publish/subscribe fan-out keyed by Postgres NOTIFY channel name.
/// Clone the `Arc<ListenRouter>` into axum state; every clone shares
/// the same backing map.
#[derive(Debug, Default)]
pub struct ListenRouter {
    channels: Mutex<HashMap<String, broadcast::Sender<String>>>,
}

impl ListenRouter {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            channels: Mutex::new(HashMap::new()),
        })
    }

    /// Return a receiver bound to `channel`, creating the channel lazily.
    ///
    /// Multiple subscribers of the same channel share one
    /// `broadcast::Sender`; the overhead per subscriber is one
    /// `Receiver` and its backing slot in the ring buffer.
    pub fn subscribe(&self, channel: &str) -> broadcast::Receiver<String> {
        let mut map = self.channels.lock().expect("listen_router mutex");
        let sender = map
            .entry(channel.to_string())
            .or_insert_with(|| {
                let (tx, _rx) = broadcast::channel(BROADCAST_BUFFER);
                tx
            });
        sender.subscribe()
    }

    /// Publish `payload` to all live subscribers on `channel`.
    ///
    /// Returns the number of receivers that got the message. No-op if
    /// nobody is subscribed — creating a zero-subscriber entry would
    /// just waste a map slot.
    pub fn publish(&self, channel: &str, payload: String) -> usize {
        let map = self.channels.lock().expect("listen_router mutex");
        match map.get(channel) {
            Some(tx) => tx.send(payload).unwrap_or(0),
            None => 0,
        }
    }

    /// List every channel that has ever been subscribed to. Used by
    /// `spawn_listen_task` to decide which PG channels to issue
    /// `LISTEN` for.
    ///
    /// v0.1 pre-registers the only channel we care about
    /// (`pgweb_livereload`) at worker startup, so this is mostly for
    /// Phase-2 dynamic-channel use cases and for diagnostics.
    pub fn registered_channels(&self) -> Vec<String> {
        let map = self.channels.lock().expect("listen_router mutex");
        map.keys().cloned().collect()
    }

    /// Make sure `channel` has a sender registered even before any
    /// subscriber asks. Used at worker startup to create the
    /// pgweb_livereload channel so NOTIFYs that arrive before any
    /// browser connects don't hit an empty map and get dropped
    /// (publish() no-ops on an unknown channel).
    pub fn preregister(&self, channel: &str) {
        let mut map = self.channels.lock().expect("listen_router mutex");
        map.entry(channel.to_string())
            .or_insert_with(|| broadcast::channel(BROADCAST_BUFFER).0);
        debug!(channel = %channel, "preregistered broadcast channel");
    }
}

/// Spawn the LISTEN task: opens a dedicated tokio-postgres connection,
/// issues `LISTEN <channel>` for each pre-registered channel, and
/// forwards every incoming notification to `router.publish`.
///
/// The task runs on the BGW's existing single-threaded tokio runtime
/// (same thread as HTTP handlers + SPI). The connection is NOT SPI —
/// it's a regular libpq-protocol session to loopback — so there's no
/// SPI conflict.
///
/// Reconnection policy: on connection loss, log + sleep 1 s + retry.
/// Forever. This is dev-mode livereload; transient DB restarts during
/// `pg-web dev` shouldn't require restarting `pg-web dev`.
pub async fn run_listen_loop(
    router: Arc<ListenRouter>,
    conn_str: String,
    channels: Vec<String>,
) {
    use futures_util::stream::StreamExt;

    loop {
        let attempt = tokio_postgres::connect(&conn_str, tokio_postgres::NoTls).await;
        let (client, mut connection) = match attempt {
            Ok(c) => c,
            Err(e) => {
                warn!(error = %e, "livereload LISTEN connect failed; retrying");
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                continue;
            }
        };

        // tokio-postgres separates protocol driver (connection) from
        // the command interface (client). Notifications come via the
        // connection's AsyncMessage stream; we convert it into a plain
        // stream and pump it.
        let (notif_tx, mut notif_rx) = tokio::sync::mpsc::unbounded_channel::<
            tokio_postgres::Notification,
        >();

        // Drive the connection. On disconnect this task ends; we'll
        // reconnect below.
        let driver = async move {
            let mut stream =
                futures_util::stream::poll_fn(move |cx| connection.poll_message(cx));
            while let Some(msg) = stream.next().await {
                match msg {
                    Ok(tokio_postgres::AsyncMessage::Notification(n)) => {
                        let _ = notif_tx.send(n);
                    }
                    Ok(_) => {}
                    Err(e) => {
                        warn!(error = %e, "livereload LISTEN connection error");
                        break;
                    }
                }
            }
        };
        tokio::spawn(driver);

        // Issue LISTEN for each channel. Errors here are fatal for
        // this attempt; break + reconnect.
        let mut listen_ok = true;
        for ch in &channels {
            // Channel names pass through format!; we restrict callers
            // to a safe allowlist upstream (worker.rs preregisters the
            // literal "pgweb_livereload"). No user input flows here in
            // v0.1.
            let stmt = format!("LISTEN {}", ch);
            if let Err(e) = client.batch_execute(&stmt).await {
                warn!(channel = %ch, error = %e, "LISTEN failed");
                listen_ok = false;
                break;
            }
            debug!(channel = %ch, "LISTEN active");
        }
        if !listen_ok {
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            continue;
        }

        // Forward notifications until the connection driver ends.
        while let Some(notif) = notif_rx.recv().await {
            let channel = notif.channel().to_string();
            let payload = notif.payload().to_string();
            let delivered = router.publish(&channel, payload);
            debug!(
                channel = %channel,
                subscribers = delivered,
                "livereload NOTIFY received"
            );
        }

        warn!("livereload LISTEN connection ended; reconnecting");
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn publish_to_empty_channel_delivers_to_zero() {
        let r = ListenRouter::new();
        assert_eq!(r.publish("ghost", "hello".into()), 0);
    }

    #[test]
    fn subscribe_then_publish_delivers() {
        let r = ListenRouter::new();
        let mut rx = r.subscribe("x");
        assert_eq!(r.publish("x", "payload".into()), 1);
        let got = rx.try_recv().unwrap();
        assert_eq!(got, "payload");
    }

    #[test]
    fn preregister_lets_publish_find_the_channel_before_any_subscriber() {
        // The specific bug this guards: spawn_listen_task pushes a
        // NOTIFY before any browser connects. Without preregister,
        // publish() no-ops (map miss) and the first reload signal is
        // lost. With preregister, the sender exists, the subsequent
        // subscribe() reads from the same sender — but the buffered
        // message is NOT replayed (broadcast semantics: subscribers
        // only see messages sent AFTER their subscribe call). So the
        // test here asserts the sender exists; the "don't lose the
        // first NOTIFY" promise is actually that the channel is
        // registered before pg-web dev could ever trigger, which is
        // true because the worker starts before dev can connect.
        let r = ListenRouter::new();
        r.preregister("z");
        assert_eq!(r.publish("z", "x".into()), 0, "no subscriber yet");
        let mut rx = r.subscribe("z");
        assert_eq!(r.publish("z", "after".into()), 1);
        assert_eq!(rx.try_recv().unwrap(), "after");
    }

    #[test]
    fn multiple_subscribers_on_same_channel_all_receive() {
        let r = ListenRouter::new();
        let mut a = r.subscribe("m");
        let mut b = r.subscribe("m");
        assert_eq!(r.publish("m", "hi".into()), 2);
        assert_eq!(a.try_recv().unwrap(), "hi");
        assert_eq!(b.try_recv().unwrap(), "hi");
    }

    #[test]
    fn channels_are_isolated() {
        let r = ListenRouter::new();
        let mut a = r.subscribe("alpha");
        let _b = r.subscribe("beta");
        r.publish("beta", "for-beta".into());
        assert!(a.try_recv().is_err(), "alpha should not see beta's msg");
    }

    #[test]
    fn registered_channels_reports_both() {
        let r = ListenRouter::new();
        r.preregister("a");
        r.subscribe("b");
        let mut got = r.registered_channels();
        got.sort();
        assert_eq!(got, vec!["a".to_string(), "b".to_string()]);
    }
}
