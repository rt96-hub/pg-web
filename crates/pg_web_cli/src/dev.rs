//! `pg-web dev` — file-watcher-driven re-push loop.
//!
//! Replicates the Vite / Next / chokidar architecture pinned in the
//! 2026-04-20 decision-log entry:
//!
//! ```text
//! native OS watcher (notify)
//!   → write-finish debounce (notify-debouncer-full, 200ms)
//!     → content-hash dedupe (Blake3)
//!       → include/exclude filter
//!         → full push
//! ```
//!
//! Missing versus Vite: no browser push (WebSocket/SSE). Save → DB sync
//! still requires the user to hit F5. That's tracked as an M1.4 follow-up
//! — see `docs/ROADMAP.md` "Browser live-reload push".
//!
//! Per-save pipeline:
//! 1. Classify each event path. Non-`pages/*.{sql,html}` and non-`public/**`
//!    paths are ignored; so are editor turds (dotfile prefix, `~`/`.tmp`/
//!    `.new`/`.bak` suffix). Keeps the debouncer output quiet so a vim save
//!    that writes `.foo.sql.swp` then renames to `foo.sql` doesn't double-push.
//! 2. Hash the file with Blake3 and compare to the last-recorded hash for
//!    that path. If the bytes are identical to the last push, skip — this is
//!    the "touch-save" / "rewrite-with-same-content" case Vite avoids via
//!    its module graph.
//! 3. For changed `.sql` handlers under `pages/`, run a shift-left preflight
//!    (`BEGIN; <file>; ROLLBACK;`) before the real push starts. A PG parse or
//!    planning error surfaces immediately and the live route stays intact.
//! 4. Call the standard `push::push` to upsert routes/templates/handlers in
//!    one transaction. Push is idempotent so "full push on any change" is
//!    fine at Phase 1 route counts; file-scoped push is a future optimization.
//!
//! Container logs are tailed inline by default (toggle with `--no-logs`);
//! a background thread reads from `docker compose logs -f` and prints each
//! line prefixed with `[pg]` so it interleaves cleanly with our own messages.

use std::collections::HashMap;
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::thread;
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use notify_debouncer_full::notify::{EventKind, RecursiveMode, Watcher};
use notify_debouncer_full::{new_debouncer, DebouncedEvent};
use postgres::{Client, NoTls};

use crate::{push, stack};

/// 200ms sits between Vite (≈100ms) and Next (≈300ms). Long enough that a
/// rename-over-write editor save collapses into one event, short enough
/// that interactive tweaking feels live.
const DEBOUNCE_WINDOW: Duration = Duration::from_millis(200);

/// How often the main loop wakes up while idle to check the Ctrl-C flag.
/// Also doubles as an upper bound on shutdown latency.
const SHUTDOWN_POLL: Duration = Duration::from_millis(500);

/// Classification of a filesystem event path.
#[derive(Debug, PartialEq, Eq)]
pub enum Action {
    /// Path is something we care about — run a push.
    Push,
    /// Path is outside the watched tree, has the wrong extension, or
    /// matches a well-known editor-turd pattern.
    Ignore,
}

/// `pg-web dev` entry point. Brings the stack up, installs a Ctrl-C
/// handler, optionally tails logs, then drops into [`watch`] until stop.
pub fn dev(app_dir: &Path, tail_logs: bool) -> Result<()> {
    // `stack::up` is idempotent — `docker compose up -d` against an
    // already-running stack is a ~1s no-op. Simpler than pre-checking.
    let url = stack::up(app_dir)?;
    println!("✓ stack up — DATABASE_URL={url}");

    let stop = Arc::new(AtomicBool::new(false));
    {
        let stop = stop.clone();
        ctrlc::set_handler(move || stop.store(true, Ordering::SeqCst))
            .context("installing Ctrl-C handler")?;
    }

    let logs_child = if tail_logs {
        Some(spawn_logs_tail(app_dir)?)
    } else {
        None
    };

    let result = watch(app_dir, &url, stop);

    if let Some(mut c) = logs_child {
        let _ = c.kill();
        let _ = c.wait();
    }
    println!("\n✓ stopped");
    result
}

/// Core watcher loop — set up the debouncer on pages/ + public/ and run
/// the event loop until `stop` is raised. Public so integration tests
/// can drive it against a testcontainer-provided URL without needing a
/// real `docker compose` stack.
pub fn watch(app_dir: &Path, url: &str, stop: Arc<AtomicBool>) -> Result<()> {
    let (tx, rx) = mpsc::channel();
    let mut debouncer =
        new_debouncer(DEBOUNCE_WINDOW, None, tx).context("creating file watcher")?;
    for sub in &["pages", "public"] {
        let target = app_dir.join(sub);
        if target.is_dir() {
            debouncer
                .watcher()
                .watch(&target, RecursiveMode::Recursive)
                .with_context(|| format!("watching {}", target.display()))?;
        }
    }
    println!("⟳ watching pages/ + public/ — edit files to re-push, Ctrl-C to stop");

    let result = event_loop(&rx, app_dir, url, &stop);
    drop(debouncer);
    result
}

fn event_loop(
    rx: &mpsc::Receiver<notify_debouncer_full::DebounceEventResult>,
    app_dir: &Path,
    url: &str,
    stop: &AtomicBool,
) -> Result<()> {
    let mut hashes: HashMap<PathBuf, blake3::Hash> = HashMap::new();
    loop {
        if stop.load(Ordering::SeqCst) {
            return Ok(());
        }
        match rx.recv_timeout(SHUTDOWN_POLL) {
            Ok(Ok(events)) => {
                if let Err(e) = handle_batch(&events, app_dir, url, &mut hashes) {
                    eprintln!("✗ {e:#}");
                }
            }
            Ok(Err(errs)) => {
                for e in errs {
                    eprintln!("watcher error: {e}");
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                bail!("watcher channel disconnected unexpectedly");
            }
        }
    }
}

fn handle_batch(
    events: &[DebouncedEvent],
    app_dir: &Path,
    url: &str,
    hashes: &mut HashMap<PathBuf, blake3::Hash>,
) -> Result<()> {
    let mut changed_any = false;
    let mut changed_sql: Vec<PathBuf> = Vec::new();

    for ev in events {
        for p in &ev.event.paths {
            if classify(p, app_dir) == Action::Ignore {
                continue;
            }

            // Removals: drop the hash so a re-created file at the same
            // path will count as new. Push is upsert-only today so stale
            // routes linger after deletions — a pre-existing push gap,
            // not a watcher bug.
            if matches!(ev.event.kind, EventKind::Remove(_)) {
                hashes.remove(p);
                changed_any = true;
                continue;
            }

            // Read + hash. A "file vanished between event and read" race
            // is treated as a change; the next remove event cleans up.
            let content = match fs::read(p) {
                Ok(c) => c,
                Err(_) => {
                    changed_any = true;
                    continue;
                }
            };
            let hash = blake3::hash(&content);
            if hashes.get(p) == Some(&hash) {
                continue;
            }
            hashes.insert(p.clone(), hash);
            changed_any = true;
            if is_pages_sql(p, app_dir) {
                changed_sql.push(p.clone());
            }
        }
    }

    if !changed_any {
        return Ok(());
    }

    // Shift-left: BEGIN; <file>; ROLLBACK; each changed handler SQL
    // before the real push starts. Abort without pushing on any error
    // so the live route keeps working while the developer fixes it.
    if !changed_sql.is_empty() {
        let mut client = Client::connect(url, NoTls).context("connecting for preflight")?;
        for path in &changed_sql {
            let sql = fs::read_to_string(path)
                .with_context(|| format!("reading {}", path.display()))?;
            if let Err(e) = preflight_sql(&mut client, &sql) {
                eprintln!("✗ preflight failed for {}: {e:#}", path.display());
                eprintln!("  (live routes unchanged; fix the SQL and save again)");
                return Ok(());
            }
        }
    }

    let summary = push::push(app_dir, url).context("push after watcher event")?;
    println!(
        "⟳ pushed — {} routes, {} templates, {} SQL files",
        summary.routes_upserted, summary.templates_upserted, summary.sql_files_executed
    );
    Ok(())
}

/// Run the file contents in a throwaway transaction — catches parse,
/// type, and function-signature errors without mutating any live route.
fn preflight_sql(client: &mut Client, sql: &str) -> Result<()> {
    let mut tx = client.transaction().context("begin preflight tx")?;
    tx.batch_execute(sql).context("executing SQL")?;
    tx.rollback().context("rolling back preflight")?;
    Ok(())
}

/// True iff `path` is an `.sql` file somewhere under `<app_dir>/pages/`.
/// Handler-SQL only — migrations/ and public/ don't preflight.
fn is_pages_sql(path: &Path, app_dir: &Path) -> bool {
    let Ok(rel) = path.strip_prefix(app_dir) else {
        return false;
    };
    let first = rel.components().next().and_then(|c| c.as_os_str().to_str());
    if first != Some("pages") {
        return false;
    }
    path.extension().and_then(|e| e.to_str()) == Some("sql")
}

/// Pure classifier for watcher events. Exposed for unit tests.
///
/// Rules:
/// - File name starting with `.` → Ignore (vim/emacs swap + dotfiles).
/// - File name ending with `~`, `.tmp`, `.new`, `.bak` → Ignore (editor turds).
/// - Path outside `<app_dir>/pages/` and `<app_dir>/public/` → Ignore.
/// - Inside `pages/`: only `.sql` / `.html` are interesting.
/// - Inside `public/`: any file.
pub fn classify(path: &Path, app_dir: &Path) -> Action {
    let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
        return Action::Ignore;
    };
    if name.starts_with('.') {
        return Action::Ignore;
    }
    if name.ends_with('~')
        || name.ends_with(".tmp")
        || name.ends_with(".new")
        || name.ends_with(".bak")
    {
        return Action::Ignore;
    }
    let Ok(rel) = path.strip_prefix(app_dir) else {
        return Action::Ignore;
    };
    let Some(first) = rel.components().next().and_then(|c| c.as_os_str().to_str()) else {
        return Action::Ignore;
    };
    match first {
        "pages" => match path.extension().and_then(|e| e.to_str()) {
            Some("sql") | Some("html") => Action::Push,
            _ => Action::Ignore,
        },
        "public" => Action::Push,
        _ => Action::Ignore,
    }
}

/// Spawn `docker compose logs -f postgres` and stream its stdout through
/// ours with a `[pg]` prefix per line. The caller owns the Child and must
/// `kill()` + `wait()` on shutdown.
fn spawn_logs_tail(app_dir: &Path) -> Result<Child> {
    let compose = stack::ensure_compose_file(app_dir)?;
    let mut child = Command::new("docker")
        .args(["compose", "-f"])
        .arg(&compose)
        .args(["logs", "-f", "--no-log-prefix", "postgres"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("spawning `docker compose logs -f postgres`")?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| anyhow!("log-tail child had no stdout pipe"))?;
    thread::spawn(move || {
        let reader = BufReader::new(stdout);
        for line in reader.lines().map_while(Result::ok) {
            println!("[pg] {line}");
        }
    });
    Ok(child)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cwd(rel: &str) -> PathBuf {
        PathBuf::from("/app").join(rel)
    }

    #[test]
    fn classify_accepts_html_under_pages() {
        assert_eq!(
            classify(&cwd("pages/index.html"), Path::new("/app")),
            Action::Push
        );
        assert_eq!(
            classify(&cwd("pages/todos/post.html"), Path::new("/app")),
            Action::Push
        );
    }

    #[test]
    fn classify_accepts_sql_under_pages() {
        assert_eq!(
            classify(&cwd("pages/index.sql"), Path::new("/app")),
            Action::Push
        );
        assert_eq!(
            classify(
                &cwd("pages/todos/toggle/post.sql"),
                Path::new("/app")
            ),
            Action::Push
        );
    }

    #[test]
    fn classify_accepts_anything_under_public() {
        assert_eq!(
            classify(&cwd("public/styles.css"), Path::new("/app")),
            Action::Push
        );
        assert_eq!(
            classify(&cwd("public/images/logo.png"), Path::new("/app")),
            Action::Push
        );
    }

    #[test]
    fn classify_rejects_non_html_non_sql_under_pages() {
        assert_eq!(
            classify(&cwd("pages/index.md"), Path::new("/app")),
            Action::Ignore
        );
        assert_eq!(
            classify(&cwd("pages/README.txt"), Path::new("/app")),
            Action::Ignore
        );
    }

    #[test]
    fn classify_rejects_dotfile_prefix() {
        assert_eq!(
            classify(&cwd("pages/.index.sql.swp"), Path::new("/app")),
            Action::Ignore
        );
        assert_eq!(
            classify(&cwd("pages/.foo.html"), Path::new("/app")),
            Action::Ignore
        );
        assert_eq!(
            classify(&cwd("public/.DS_Store"), Path::new("/app")),
            Action::Ignore
        );
    }

    #[test]
    fn classify_rejects_editor_suffixes() {
        for bad in &[
            "pages/index.sql~",
            "pages/index.html.tmp",
            "pages/index.html.new",
            "pages/index.sql.bak",
        ] {
            assert_eq!(
                classify(&cwd(bad), Path::new("/app")),
                Action::Ignore,
                "should ignore {bad}"
            );
        }
    }

    #[test]
    fn classify_rejects_outside_watched_dirs() {
        assert_eq!(
            classify(&cwd("migrations/0001_init.sql"), Path::new("/app")),
            Action::Ignore
        );
        assert_eq!(
            classify(&cwd("pgweb.toml"), Path::new("/app")),
            Action::Ignore
        );
        assert_eq!(
            classify(&cwd("target/debug/foo"), Path::new("/app")),
            Action::Ignore
        );
    }

    #[test]
    fn classify_rejects_paths_outside_app_dir() {
        assert_eq!(
            classify(Path::new("/other/pages/index.html"), Path::new("/app")),
            Action::Ignore
        );
    }

    #[test]
    fn is_pages_sql_true_for_pages_sql() {
        assert!(is_pages_sql(&cwd("pages/index.sql"), Path::new("/app")));
        assert!(is_pages_sql(
            &cwd("pages/todos/post.sql"),
            Path::new("/app")
        ));
    }

    #[test]
    fn is_pages_sql_false_for_html_or_public_or_migrations() {
        assert!(!is_pages_sql(&cwd("pages/index.html"), Path::new("/app")));
        assert!(!is_pages_sql(&cwd("public/styles.css"), Path::new("/app")));
        assert!(!is_pages_sql(
            &cwd("migrations/0001.sql"),
            Path::new("/app")
        ));
    }

    #[test]
    fn blake3_same_bytes_same_hash() {
        // Sanity — if this ever changes, dedupe breaks.
        let a = blake3::hash(b"CREATE TABLE t (id int);");
        let b = blake3::hash(b"CREATE TABLE t (id int);");
        assert_eq!(a, b);
    }

    #[test]
    fn blake3_different_bytes_different_hash() {
        let a = blake3::hash(b"CREATE TABLE t (id int);");
        let b = blake3::hash(b"CREATE TABLE t (id bigint);");
        assert_ne!(a, b);
    }
}
