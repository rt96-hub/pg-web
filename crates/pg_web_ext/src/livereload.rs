//! Dev-mode live-reload: SSE endpoint, client-side JS, script injection.
//!
//! Machinery flow:
//!   1. `pg-web dev` classifies a file change, pushes it, then
//!      `NOTIFY pgweb_livereload '{"kind":"css", ...}'`.
//!   2. The BGW's LISTEN task (listen_router::run_listen_loop) receives
//!      it and forwards to the in-memory `ListenRouter`.
//!   3. Every browser tab connected to `GET /_pgweb/livereload`
//!      receives the event via `broadcast::Receiver`.
//!   4. The injected `livereload.js` stub on that page decides what to
//!      do: cache-bust stylesheets for CSS changes, full reload for
//!      anything else.
//!
//! All three hops (extension → subscriber HashMap → SSE client) fan
//! out in memory — one `LISTEN` connection feeds N tabs.
//!
//! Scope disclaimers:
//! - Live-reload is dev-only. `pgweb.settings.env = 'production'`
//!   causes `GET /_pgweb/livereload` to 404 and the injection to
//!   skip. The LISTEN task itself is only started when env is
//!   development at worker startup (see worker.rs).
//! - The client JS is deliberately simple: no framework, no morph.
//!   Phase-2 could swap in Idiomorph for state-preserving refreshes;
//!   v0.1 picks the boring-but-correct `location.reload()` path.

use std::convert::Infallible;
use std::sync::Arc;
use std::time::Duration;

use axum::{
    extract::State,
    http::{header, StatusCode},
    response::{
        sse::{Event, KeepAlive, Sse},
        IntoResponse, Response,
    },
};
use futures_util::stream::{Stream, StreamExt};
use tokio::time::sleep;
use tokio_stream::wrappers::BroadcastStream;

use crate::listen_router::ListenRouter;
use crate::settings::Env;

/// Postgres NOTIFY channel the CLI uses to publish livereload events.
/// Same value hardcoded into `pg-web dev`'s post-push hook — the two
/// must agree. If we ever need to expose this externally, promote to
/// a crate-level const in a shared module.
pub const LIVERELOAD_CHANNEL: &str = "pgweb_livereload";

/// The client-side JS served at `/_pgweb/livereload.js`. Kept small
/// and framework-free so it can embed in any pg-web app's rendered
/// HTML without conflicts. No JS tooling required to read or modify.
///
/// Behavior:
/// - Subscribes to SSE at /_pgweb/livereload (reconnect is handled by
///   the browser's native EventSource; we don't need to reconnect
///   manually).
/// - On `reload` event: parse payload; dispatch on `kind`.
/// - `kind = "css"`: cache-bust every <link rel=stylesheet> by adding
///   a `?_pgweb_v=<timestamp>` query param. No page reload.
/// - Anything else (`"route"`, `"full"`, missing kind, parse error):
///   full `location.reload()`.
const LIVERELOAD_JS: &str = r#"(function(){
  if (typeof EventSource === 'undefined') return;

  // Sentinel prevents duplicate EventSources when the script is
  // injected multiple times (bfcache restores, rapid navigation, etc.).
  if (window.__pgwebLivereload) return;

  var es = new EventSource('/_pgweb/livereload');
  window.__pgwebLivereload = es;

  function cleanup() {
    try {
      if (es) es.close();
    } catch (e) {}
    delete window.__pgwebLivereload;
  }

  es.addEventListener('reload', function(ev){
    var msg = {};
    try { msg = JSON.parse(ev.data); } catch(e) {}
    if (msg.kind === 'css') {
      var t = Date.now();
      document.querySelectorAll('link[rel="stylesheet"]').forEach(function(link){
        try {
          var u = new URL(link.href, window.location.href);
          u.searchParams.set('_pgweb_v', t);
          link.href = u.href;
        } catch (e) {}
      });
      return;
    }
    window.location.reload();
  });

  es.addEventListener('error', function(){
    // Browser auto-reconnects; don't log — dev's restart is noisy
    // enough and connection flaps are normal when the server restarts.
  });

  // Critical bfcache + navigation hygiene.
  // pagehide is the most reliable signal that the page is being
  // discarded or frozen (including bfcache).
  window.addEventListener('pagehide', cleanup, { once: true });

  // beforeunload as a belt-and-suspenders fallback for some older
  // browsers and certain navigation scenarios.
  window.addEventListener('beforeunload', cleanup, { once: true });

  // If the page was restored from bfcache and the old connection
  // somehow survived, close it so the freshly injected script can
  // create a clean one.
  window.addEventListener('pageshow', function(ev){
    if (ev.persisted && window.__pgwebLivereload) {
      cleanup();
    }
  }, { once: true });
})();
"#;

/// Tag inserted into rendered HTML responses when livereload is on.
/// The string is a search target for the injection logic (see
/// `inject_script_if_eligible`) so we also recognize when a user has
/// manually included it and skip the automatic re-injection.
const LIVERELOAD_SCRIPT_TAG: &str =
    "<script src=\"/_pgweb/livereload.js\" async data-pgweb-livereload></script>";

/// GET `/_pgweb/livereload.js` — serves the inline client stub.
///
/// Unconditionally available (doesn't care about env) so a cached
/// reference from an old dev page doesn't 404 after a restart. The
/// SSE endpoint IS env-gated, so even if this JS loads in prod, it
/// just gets an immediate 404 on the EventSource and does nothing.
pub async fn serve_livereload_js() -> Response {
    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, "application/javascript; charset=utf-8"),
            (header::CACHE_CONTROL, "no-cache"),
        ],
        LIVERELOAD_JS,
    )
        .into_response()
}

/// GET `/_pgweb/livereload` — SSE stream that forwards NOTIFY payloads
/// received on the `pgweb_livereload` channel.
///
/// 404s in production mode so a stray script load doesn't hold an
/// unnecessary connection open. In development, returns a
/// `text/event-stream` that stays open forever; the browser's
/// EventSource handles reconnection on drop.
pub async fn serve_livereload_sse(
    State(router): State<Arc<ListenRouter>>,
) -> Response {
    // Env check on every connection. `transaction` is the SPI-safe
    // wrapper already used elsewhere in http.rs.
    let env = crate::cache::current_env();
    if env != Env::Development {
        return (StatusCode::NOT_FOUND, "live-reload is development only\n")
            .into_response();
    }

    let rx = router.subscribe(LIVERELOAD_CHANNEL);
    let stream = build_reload_stream(rx);

    // Hard safety net for dev-only SSE connections.
    // Even with perfect client cleanup, bfcache edge cases, crashed
    // tabs, or very long-running dev sessions can leave connections
    // open. After 2 hours we unilaterally close the stream. The
    // browser will see the connection drop and stop trying.
    //
    // This is deliberately generous (a full workday) and only affects
    // the livereload endpoint (which 404s in production).
    //
    // We also stop promptly on graceful shutdown (request_shutdown from
    // the pgrx SIGTERM poller) so the streams don't hold the worker open
    // during pg_ctl stop / docker stop (prompt 016).
    let max_lifetime = sleep(Duration::from_secs(2 * 60 * 60));
    let shutdown = router.wait_shutdown();
    let stop = async {
        tokio::select! {
            _ = max_lifetime => {}
            _ = shutdown => {}
        }
    };
    let stream = stream.take_until(stop);

    Sse::new(stream)
        .keep_alive(KeepAlive::new().interval(Duration::from_secs(30)))
        .into_response()
}

/// Convert a broadcast::Receiver into an SSE Event stream. Lagged
/// receivers (dropped messages due to a slow consumer) are skipped
/// silently — a live-reload that missed an event will get the next
/// one, and the buffer is deep enough that this is rare in practice.
fn build_reload_stream(
    rx: tokio::sync::broadcast::Receiver<String>,
) -> impl Stream<Item = Result<Event, Infallible>> {
    BroadcastStream::new(rx).filter_map(|res| async move {
        match res {
            Ok(payload) => Some(Ok(Event::default().event("reload").data(payload))),
            Err(_) => None,
        }
    })
}

/// If the response body looks like a full HTML document AND env is
/// development AND the script isn't already there, splice the
/// livereload script before `</body>`. Fragment responses (no closing
/// body tag) are left alone — HTMX OOB swaps shouldn't carry the
/// injected tag.
///
/// Called from `http::handle` for `ServeOutcome::Response` bodies.
/// `env` is passed in to avoid a second SPI call per request.
pub fn inject_script_if_eligible(body: String, env: Env) -> String {
    if env != Env::Development {
        return body;
    }
    // Idempotent: user who manually included the script sees no
    // double-injection. Looking for `data-pgweb-livereload` is a
    // stable marker.
    if body.contains("data-pgweb-livereload") {
        return body;
    }
    // Only touch bodies that close a <body> element — that's the
    // heuristic for "full HTML document" vs "fragment response".
    match body.rfind("</body>") {
        Some(idx) => {
            let mut out = String::with_capacity(body.len() + LIVERELOAD_SCRIPT_TAG.len());
            out.push_str(&body[..idx]);
            out.push_str(LIVERELOAD_SCRIPT_TAG);
            out.push_str(&body[idx..]);
            out
        }
        None => body,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inject_skips_production() {
        let body = "<html><body>hi</body></html>".to_string();
        let out = inject_script_if_eligible(body.clone(), Env::Production);
        assert_eq!(out, body, "prod mode must not inject");
    }

    #[test]
    fn inject_skips_fragments_without_body_tag() {
        // HTMX post responses look like `<li>new</li>` — no </body>.
        // They must pass through unchanged or HTMX OOB swaps would
        // ship the livereload tag around with them.
        let frag = "<li>new todo</li>".to_string();
        let out = inject_script_if_eligible(frag.clone(), Env::Development);
        assert_eq!(out, frag);
    }

    #[test]
    fn inject_places_script_right_before_body_close() {
        let body = "<!doctype html><html><body><h1>x</h1></body></html>".to_string();
        let out = inject_script_if_eligible(body, Env::Development);
        assert!(
            out.contains("data-pgweb-livereload"),
            "should have injected: {out}"
        );
        let tag_pos = out.find("data-pgweb-livereload").unwrap();
        let body_close = out.find("</body>").unwrap();
        assert!(
            tag_pos < body_close,
            "script must appear before </body>, got: {out}"
        );
    }

    #[test]
    fn inject_is_idempotent_when_script_already_present() {
        let body = format!(
            "<html><body>{}</body></html>",
            LIVERELOAD_SCRIPT_TAG
        );
        let out = inject_script_if_eligible(body.clone(), Env::Development);
        assert_eq!(out, body, "already-injected body should be untouched");
        // Double-check only one copy exists.
        let count = out.matches("data-pgweb-livereload").count();
        assert_eq!(count, 1);
    }

    #[test]
    fn inject_uses_last_body_close() {
        // If the document contains the literal text `</body>` inside a
        // string or comment earlier, the injection should still land
        // before the REAL closing tag — we use rfind so this works by
        // construction. Regression-guard the behavior.
        let body = "<html><body><pre>prev: </body></pre></body></html>".to_string();
        let out = inject_script_if_eligible(body, Env::Development);
        let script_pos = out.find("data-pgweb-livereload").unwrap();
        let last_body_close = out.rfind("</body>").unwrap();
        assert!(script_pos < last_body_close);
        // And only one injection happened.
        assert_eq!(out.matches("data-pgweb-livereload").count(), 1);
    }

    #[test]
    fn livereload_js_contains_bfcache_cleanup() {
        // Regression guard for the SSE connection leak fix.
        // The client JS must contain the defensive lifecycle code so
        // that future edits don't accidentally re-introduce the
        // accumulation bug under rapid navigation + bfcache.
        assert!(
            LIVERELOAD_JS.contains("pagehide"),
            "JS must listen for pagehide to close EventSource"
        );
        assert!(
            LIVERELOAD_JS.contains("beforeunload"),
            "JS must listen for beforeunload as fallback"
        );
        assert!(
            LIVERELOAD_JS.contains("pageshow"),
            "JS must handle pageshow + persisted for bfcache restores"
        );
        assert!(
            LIVERELOAD_JS.contains("__pgwebLivereload"),
            "JS must use a sentinel to prevent duplicate EventSources"
        );
        assert!(
            LIVERELOAD_JS.contains("es.close()"),
            "JS must actually call close() on the EventSource"
        );
    }
}
