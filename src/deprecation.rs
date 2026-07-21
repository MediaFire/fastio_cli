//! One-time deprecation notices.
//!
//! When a command group or individual command is superseded, these helpers
//! steer users toward the replacement without spamming a notice on every
//! invocation: [`warn_legacy`] prints a single `[legacy]` stderr line per
//! group per process, and [`warn_once`] prints a single free-form notice per
//! key per process (used for individually re-pointed commands such as
//! `ripley search`).
//!
//! The registry lives in the library crate so the built-in MCP server can
//! reuse the same de-duplication keys; MCP handlers append a one-line
//! markdown notice to their response instead of writing to stderr (callers
//! decide the surface — this module only owns the once-per-key gate and the
//! stderr-printing convenience).

use std::collections::HashSet;
use std::sync::{Mutex, OnceLock};

use colored::Colorize;

/// Process-global set of legacy group keys that have already emitted a notice.
///
/// Keyed by the static group name (e.g. `"ripley-search"`). Wrapped in a [`Mutex`] for
/// thread-safe concurrent access from both CLI dispatch and MCP handlers.
static WARNED_GROUPS: OnceLock<Mutex<HashSet<&'static str>>> = OnceLock::new();

/// Record that `group` has been warned, returning `true` if this call is the
/// first to do so (i.e. the caller should emit the notice).
///
/// On a poisoned mutex this recovers the guard via [`std::sync::PoisonError::into_inner`]
/// rather than panicking — a poisoned lock here only means another thread
/// panicked while holding it, and the worst case of proceeding is a duplicate
/// notice, never unsound data. This deliberately avoids `unwrap()`/`expect()`
/// per the project's no-panic-in-production rule.
fn mark_warned(group: &'static str) -> bool {
    let mutex = WARNED_GROUPS.get_or_init(|| Mutex::new(HashSet::new()));
    let mut guard = match mutex.lock() {
        Ok(g) => g,
        Err(poisoned) => poisoned.into_inner(),
    };
    guard.insert(group)
}

/// Emit a one-time `[legacy]` deprecation notice for `group` to stderr.
///
/// The notice fires at most once per `group` per process and points the user
/// at `replacement` (a short hint such as `"fastio ripley"`). It is:
///
/// - **Suppressed under `--quiet`** (`quiet == true`) — no notice, and the
///   once-per-key gate is NOT consumed, so a later non-quiet call still warns.
/// - **NOT suppressed** for non-TTY output or `--format json`: the notice goes
///   to stderr only, so it never corrupts machine-readable stdout, and an
///   agent reading stderr benefits from the steer.
///
/// Returns `true` if a notice was actually printed (useful for callers that
/// want to mirror the notice into another channel, e.g. MCP responses).
///
/// Thread-safe: concurrent calls for the same `group` print exactly once.
// Not `#[must_use]`: this is primarily a side-effecting notice; most CLI
// dispatch callers fire it and ignore the bool. The return value is an opt-in
// signal for the MCP path that wants to mirror the notice.
#[allow(clippy::must_use_candidate)]
pub fn warn_legacy(group: &'static str, replacement: &str, quiet: bool) -> bool {
    if quiet {
        return false;
    }
    if !mark_warned(group) {
        return false;
    }
    eprintln!(
        "{} `{}` is a legacy command group, superseded by {}; it remains \
         functional for now.",
        "[legacy]".yellow().bold(),
        group,
        replacement,
    );
    true
}

/// Emit a one-time free-form deprecation `message` to stderr, de-duplicated
/// by `key`.
///
/// Unlike [`warn_legacy`] (which prints a fixed `[legacy]`-prefixed sentence
/// for whole command groups), this lets a caller supply its own wording —
/// used for individual re-pointed commands such as `ripley search`, whose
/// notice steers to `files search` / `ripley ask` rather than to
/// `fastio ripley`. Same guarantees: at most once per `key` per process,
/// suppressed under `quiet` (and the gate is not consumed when suppressed),
/// stderr-only so machine-readable stdout is never corrupted, thread-safe.
///
/// Returns `true` if a notice was actually printed.
#[allow(clippy::must_use_candidate)]
pub fn warn_once(key: &'static str, message: &str, quiet: bool) -> bool {
    if quiet {
        return false;
    }
    if !mark_warned(key) {
        return false;
    }
    eprintln!("{} {}", "[deprecated]".yellow().bold(), message);
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Barrier;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Reset the registry between tests. Tests run in-process and share the
    /// static, so each test that asserts "first call" semantics must start
    /// from a clean slate for its own unique key.
    fn fresh_key(key: &'static str) {
        let mutex = WARNED_GROUPS.get_or_init(|| Mutex::new(HashSet::new()));
        let mut guard = match mutex.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };
        guard.remove(key);
    }

    #[test]
    fn warn_once_fires_once_and_respects_quiet() {
        fresh_key("warn-once-test");
        // Quiet suppresses AND does not consume the gate.
        assert!(!warn_once("warn-once-test", "use the new thing", true));
        // First non-quiet call prints.
        assert!(warn_once("warn-once-test", "use the new thing", false));
        // Second call is de-duplicated.
        assert!(!warn_once("warn-once-test", "use the new thing", false));
    }

    #[test]
    fn warn_legacy_fires_once_per_key() {
        fresh_key("once-test");
        assert!(
            warn_legacy("once-test", "fastio ripley", false),
            "first call should print"
        );
        assert!(
            !warn_legacy("once-test", "fastio ripley", false),
            "second call should be suppressed"
        );
        assert!(
            !warn_legacy("once-test", "fastio ripley", false),
            "third call should be suppressed"
        );
    }

    #[test]
    fn warn_legacy_silent_under_quiet() {
        fresh_key("quiet-test");
        assert!(
            !warn_legacy("quiet-test", "fastio ripley", true),
            "quiet call must not print"
        );
        // The quiet call must NOT consume the once-gate, so a later
        // non-quiet call still warns.
        assert!(
            warn_legacy("quiet-test", "fastio ripley", false),
            "non-quiet call after a quiet one should still print"
        );
    }

    #[test]
    fn warn_legacy_distinct_keys_independent() {
        fresh_key("key-a");
        fresh_key("key-b");
        assert!(warn_legacy("key-a", "x", false));
        assert!(warn_legacy("key-b", "x", false));
        assert!(!warn_legacy("key-a", "x", false));
        assert!(!warn_legacy("key-b", "x", false));
    }

    #[test]
    fn warn_legacy_thread_safe_prints_once() {
        const THREADS: usize = 16;
        fresh_key("concurrent-test");
        let barrier = std::sync::Arc::new(Barrier::new(THREADS));
        let printed = std::sync::Arc::new(AtomicUsize::new(0));
        let mut handles = Vec::with_capacity(THREADS);
        for _ in 0..THREADS {
            let barrier = std::sync::Arc::clone(&barrier);
            let printed = std::sync::Arc::clone(&printed);
            handles.push(std::thread::spawn(move || {
                barrier.wait();
                if warn_legacy("concurrent-test", "fastio ripley", false) {
                    printed.fetch_add(1, Ordering::SeqCst);
                }
            }));
        }
        for h in handles {
            // Test code may unwrap per the project rule; a panicked worker
            // thread is itself a test failure we want surfaced.
            h.join().expect("worker thread panicked");
        }
        assert_eq!(
            printed.load(Ordering::SeqCst),
            1,
            "exactly one thread should have printed"
        );
    }

    #[test]
    fn warn_legacy_poison_safe() {
        // Poison the mutex by panicking while holding the lock, then verify a
        // subsequent `warn_legacy` recovers instead of panicking.
        fresh_key("poison-test");
        let mutex = WARNED_GROUPS.get_or_init(|| Mutex::new(HashSet::new()));
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = mutex.lock().expect("lock for poisoning");
            panic!("intentional poison");
        }));
        assert!(result.is_err(), "the panic should have unwound");
        assert!(mutex.is_poisoned(), "mutex should be poisoned");
        // Must not panic despite the poisoned lock.
        let first = warn_legacy("poison-test", "fastio ripley", false);
        let second = warn_legacy("poison-test", "fastio ripley", false);
        assert!(first, "first post-poison call should print");
        assert!(!second, "second post-poison call should be suppressed");
    }
}
