//! The one place *this workspace* reads the process environment, and
//! the only way a test changes what it says.
//!
//! Not the only place the process reads it: `tracing-subscriber`
//! reads `RUST_LOG`, `dirs` reads `HOME` / `XDG_CONFIG_HOME` /
//! `APPDATA` under `credentials_file()`, and `reqwest` reads
//! `HTTP_PROXY` / `NO_PROXY` when it builds a client. Those are
//! inside dependencies, so neither the lint nor a test overlay
//! reaches them: overlaying `HOME` compiles, runs, and changes
//! nothing.
//!
//! # Why this is not `std::env`
//!
//! Tests used to set up environment-dependent behaviour with
//! `temp_env`, which mutates the process environment for the duration
//! of a closure. `setenv` is `unsafe` since Rust 1.80 for a concrete
//! reason: on Unix it can reallocate `environ` while another thread is
//! inside `getenv`, and that is a use-after-free, not a race whose
//! worst case is a wrong value. A libtest binary runs its tests on
//! many threads at once, and the concurrent reader does not have to be
//! ours — `std::env::temp_dir` (every `tempfile::tempdir()`),
//! `Command::spawn` building a child's environment, and any dependency
//! that consults the environment are all `getenv` on another thread.
//! `temp_env`'s own lock cannot serialise against any of them; it only
//! serialises against other `temp_env` calls.
//!
//! So nothing in this workspace mutates the process environment, in
//! tests or anywhere else. Production reads it through [`var`],
//! [`var_os`] and [`var_non_empty`]; a test replaces what those return
//! with [`testing::with_vars`], which touches no global state at all.
//!
//! # What a test overlay does
//!
//! [`testing::with_vars`] installs a **replacement** environment on the
//! current thread. While it is installed, the process environment is
//! invisible: a name the test did not list reads as unset. That is the
//! point — it is what makes a test that reads `GITHUB_ACTIONS` behave
//! the same on a laptop and on a GitHub Actions runner, without a
//! hand-maintained list of variables to scrub first.
//!
//! The overlay covers *our* reads, and only ours. A dependency that
//! consults the environment (`dirs`, `keyring`, `reqwest`'s proxy
//! variables) and any process we spawn both see the real one, and
//! they see it silently — an overlay that sets `HOME` or `PATH`
//! changes nothing for them. Setting a variable for a child is a
//! different job with a different tool: `Command::env`, which touches
//! no shared state and is not affected by any of this.
//!
//! Keys match exactly. On Windows the real environment does not —
//! `std::env::var_os("PATH")` finds a `Path` — so an overlay on
//! `Path` leaves `var("PATH")` reading the host's. Nothing here reads
//! a name in a case other than the one it writes, so no call site can
//! tell the difference, and matching the platform would mean a
//! Windows-only normalisation that this suite never runs: the unit
//! tests are a Linux and macOS job, Windows only builds the binary.
//!
//! The overlay is thread-local, and it is compiled into release builds
//! rather than gated behind `cfg(test)`. `cfg(test)` cannot do the job:
//! it is false whenever this crate is compiled as a dependency, so
//! every consumer crate's tests would read the real environment
//! anyway. `mergify_tui::theme` carries the same note for the same
//! reason. The cost is one thread-local read per environment lookup on
//! a path that runs a handful of times per process.
//!
//! # The empty-string rule
//!
//! The CLI's resolver pattern (`flag → env → default`) treats the
//! empty string as "not set", because callers in the wild — most
//! notably the `gha-mergify-ci` GitHub Action — `export VAR=""`
//! when no value is available. Inlining
//! `var(NAME).filter(|s| !s.is_empty())` on every call site looks
//! innocuous but invites the same bug we've now hit twice
//! (monorepo#33423, `MERGIFY_CONFIG_PATH` and
//! `MERGIFY_TEST_EXIT_CODE`): a contributor adds clap's
//! `env = "MERGIFY_FOO"` attribute on a flag instead, and clap's
//! parser treats an empty env value as a present-but-empty flag
//! value, aborting parsing before any of our code can fall back.
//!
//! Use [`var_non_empty`] for the env-var leg of any `flag → env
//! → default` chain. Do **not** wire env vars through clap's
//! `env = ...` attribute for any of the `MERGIFY_*` namespace.

use std::cell::Cell;
use std::cell::RefCell;
use std::collections::HashMap;
use std::ffi::OsStr;
use std::ffi::OsString;

thread_local! {
    /// The replacement environment installed by
    /// [`testing::with_vars`], or `None` when the real process
    /// environment is in effect. `Some(map)` is exhaustive: names
    /// absent from `map` read as unset.
    static OVERLAY: RefCell<Option<HashMap<OsString, OsString>>> = const { RefCell::new(None) };

    /// How many overlay scopes are open on this thread. Each scope
    /// records the value it pushed and checks it again on the way
    /// out, so two overlapping scopes are a panic rather than a
    /// wrong answer — see `testing::Restore`'s `Drop`.
    static DEPTH: Cell<u64> = const { Cell::new(0) };
}

/// Read an environment variable as an `OsString`.
///
/// Returns the test overlay's value when one is installed on this
/// thread (see the module docs), otherwise the process environment's.
#[must_use]
pub fn var_os(name: &str) -> Option<OsString> {
    // `try_with`, not `with`: `OVERLAY` owns a `HashMap` and so
    // registers a thread-local destructor, and `with` panics once
    // that has run. A `Drop` impl reading a variable while its thread
    // tears down would abort the process, which `std::env::var_os`
    // never did. No overlay can exist at that point anyway.
    OVERLAY
        .try_with(|slot| {
            slot.borrow().as_ref().map(|map| {
                // An installed overlay *is* the environment: a name
                // it does not carry reads as unset.
                map.get(OsStr::new(name)).cloned()
            })
        })
        .unwrap_or(None)
        .unwrap_or_else(|| {
            // The single sanctioned process-environment read in
            // the workspace: every other caller goes through the
            // functions above.
            #[allow(clippy::disallowed_methods)]
            std::env::var_os(name)
        })
}

/// Read an environment variable as a `String`.
///
/// A value that is not valid UTF-8 reads as unset, matching
/// `std::env::var(name).ok()`.
#[must_use]
pub fn var(name: &str) -> Option<String> {
    var_os(name).and_then(|value| value.into_string().ok())
}

/// [`var_non_empty`] for a value that need not be UTF-8 — the editor
/// chain (`GIT_EDITOR` / `VISUAL` / `EDITOR`) hands its value
/// straight to a `Command`, so decoding it would only be to re-encode
/// it.
#[must_use]
pub fn var_os_non_empty(name: &str) -> Option<OsString> {
    var_os(name).filter(|value| !value.is_empty())
}

/// Read an environment variable and return its value if it's set
/// to a non-empty string. Unset or empty both collapse to `None`.
///
/// This is the standard primitive for the env-var leg of the
/// `--flag → env → default` resolver chain across every ported
/// command. See the module doc for the empty-as-unset rationale.
#[must_use]
pub fn var_non_empty(name: &str) -> Option<String> {
    var(name).filter(|value| !value.is_empty())
}

/// Give a test environment-dependent behaviour without mutating the
/// process environment.
///
/// Compiled unconditionally, not behind `cfg(test)` or a feature —
/// see the module docs for why that is the only thing that works.
pub mod testing {
    use std::collections::HashMap;
    use std::ffi::OsStr;
    use std::ffi::OsString;
    use std::future::Future;
    use std::marker::PhantomData;
    use std::rc::Rc;

    use super::DEPTH;
    use super::OVERLAY;

    /// Restores the enclosing overlay (usually `None`) when the scope
    /// ends, including while a panic unwinds.
    ///
    /// Deliberately `!Send` (the `Rc` marker). The overlay lives on
    /// the thread that installed it, so a guard that reached another
    /// thread would restore the wrong thread's slot and strand the
    /// overlay on the original one — permanently, since nothing else
    /// clears it. Making the guard unsendable makes the future
    /// returned by [`with_vars_async`] unsendable too, so
    /// `tokio::spawn`-ing it is a compile error rather than a silent
    /// leak.
    struct Restore {
        previous: Option<HashMap<OsString, OsString>>,
        /// The value this scope pushed onto `DEPTH`, checked again on
        /// the way out.
        depth: u64,
        _unsend: PhantomData<Rc<()>>,
    }

    impl Restore {
        /// Make `map` this thread's environment until the returned
        /// guard drops.
        fn install_map(map: HashMap<OsString, OsString>) -> Self {
            let depth = DEPTH.with(|open| {
                let pushed = open.get() + 1;
                open.set(pushed);
                pushed
            });
            Self {
                previous: OVERLAY.with_borrow_mut(|slot| slot.replace(map)),
                depth,
                _unsend: PhantomData,
            }
        }
    }

    impl Drop for Restore {
        fn drop(&mut self) {
            // Ignore a thread already past its destructors: there is
            // nothing left to restore, and panicking in a `Drop`
            // during unwinding aborts.
            let _ = OVERLAY.try_with(|slot| *slot.borrow_mut() = self.previous.take());
            let Ok(innermost) = DEPTH.try_with(|open| {
                let innermost = open.get();
                open.set(self.depth - 1);
                innermost
            }) else {
                return;
            };
            // Never report while already unwinding: a panic inside a
            // `Drop` during a panic aborts the process.
            if std::thread::panicking() {
                return;
            }
            // Scopes have to close in the order they opened, and the
            // sync helpers cannot do otherwise: the guard is a local.
            // `join!` of two `with_vars_async` on one thread can —
            // each future holds its guard across an await, so the
            // first to finish restores the state saved before the
            // second, and the second resumes without its variables.
            // Restoring `previous` above has already done that damage
            // by the time we get here; what this turns into a failure
            // is the silence.
            assert_eq!(
                innermost, self.depth,
                "environment overlay scopes closed out of order: two \
                 overlays overlap on this thread. Wrap the `join!`, \
                 do not wrap each arm."
            );
        }
    }

    fn install<K, V, I>(vars: I) -> Restore
    where
        I: IntoIterator<Item = (K, Option<V>)>,
        K: AsRef<OsStr>,
        V: AsRef<OsStr>,
    {
        // Nesting layers onto the enclosing overlay rather than
        // replacing it, so an inner `with_var` reads like the
        // process-environment version it replaces.
        let mut map = OVERLAY.with_borrow(|slot| slot.clone().unwrap_or_default());
        for (name, value) in vars {
            match value {
                Some(value) => {
                    map.insert(name.as_ref().to_os_string(), value.as_ref().to_os_string());
                }
                None => {
                    map.remove(name.as_ref());
                }
            }
        }
        Restore::install_map(map)
    }

    /// Run `f` with `vars` as the entire environment, as far as
    /// [`super::var`] and friends are concerned.
    ///
    /// A `None` value means "unset", which only matters when nesting:
    /// at the outermost level every name not listed is already unset.
    pub fn with_vars<K, V, I, F, R>(vars: I, f: F) -> R
    where
        I: IntoIterator<Item = (K, Option<V>)>,
        K: AsRef<OsStr>,
        V: AsRef<OsStr>,
        F: FnOnce() -> R,
    {
        let _restore = install(vars);
        f()
    }

    /// The empty environment: every name reads as unset.
    ///
    /// What a case whose whole point is "none of this is configured"
    /// wants. Unlike [`with_vars`], this does *not* inherit an
    /// enclosing overlay — an empty `with_vars` would be a no-op
    /// inside one, which is the opposite of what the name promises.
    pub fn with_no_vars<F, R>(f: F) -> R
    where
        F: FnOnce() -> R,
    {
        let _restore = Restore::install_map(HashMap::new());
        f()
    }

    /// [`with_no_vars`] for an `async` body.
    pub async fn with_no_vars_async<Fut>(future: Fut) -> Fut::Output
    where
        Fut: Future,
    {
        let _restore = Restore::install_map(HashMap::new());
        future.await
    }

    /// [`with_vars`] for a single variable.
    pub fn with_var<K, V, F, R>(name: K, value: Option<V>, f: F) -> R
    where
        K: AsRef<OsStr>,
        V: AsRef<OsStr>,
        F: FnOnce() -> R,
    {
        with_vars([(name, value)], f)
    }

    /// [`with_vars`] for an `async` body.
    ///
    /// The overlay lives on the thread that polls `future`. The
    /// returned future is `!Send`, so `tokio::spawn`ing *it* is a
    /// compile error rather than a silent read of the real
    /// environment.
    ///
    /// Work the body hands to another thread is a different matter
    /// and is not guarded: `spawn_blocking`, `std::thread::spawn`,
    /// and `tokio::spawn` / `JoinSet::spawn` on a `multi_thread`
    /// runtime all run where there is no overlay. Every
    /// `#[tokio::test]` here is the default `current_thread` flavor,
    /// where a spawned task stays on this thread and does see it;
    /// adding `flavor = "multi_thread"` to a test that spawns under
    /// an overlay would silently start reading the host's
    /// environment. Such a test needs the value passed in.
    ///
    /// # Panics
    ///
    /// Guards must also nest. Two overlay scopes overlapping on one
    /// thread (`join!` of two `with_vars_async`) would see each
    /// other's variables and restore in the wrong order, so the
    /// second scope to close panics instead. Wrap the `join!`, don't
    /// wrap each arm.
    pub async fn with_vars_async<K, V, I, Fut>(vars: I, future: Fut) -> Fut::Output
    where
        I: IntoIterator<Item = (K, Option<V>)>,
        K: AsRef<OsStr>,
        V: AsRef<OsStr>,
        Fut: Future,
    {
        let _restore = install(vars);
        future.await
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        #[should_panic(expected = "closed out of order")]
        fn overlapping_scopes_panic_instead_of_answering_wrongly() {
            // What `join!`-ing two `with_vars_async` arms does, with
            // the awaits taken out: the outer guard drops first.
            let outer = install([("MERGIFY_TEST_A", Some("outer"))]);
            let inner = install([("MERGIFY_TEST_B", Some("inner"))]);
            drop(outer);
            drop(inner);
        }

        #[test]
        fn nesting_in_order_does_not_panic() {
            let outer = install([("MERGIFY_TEST_A", Some("outer"))]);
            let inner = install([("MERGIFY_TEST_B", Some("inner"))]);
            drop(inner);
            drop(outer);
            assert_eq!(super::super::var("MERGIFY_TEST_A"), None);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn returns_some_for_non_empty_value() {
        let got = testing::with_var("MERGIFY_TEST_HELPER_X", Some("hello"), || {
            var_non_empty("MERGIFY_TEST_HELPER_X")
        });
        assert_eq!(got.as_deref(), Some("hello"));
    }

    #[test]
    fn returns_none_for_empty_value() {
        // The whole point of this helper: an empty env var is
        // treated as if it were not set. Regression-prone enough
        // that we pin it explicitly.
        let got = testing::with_var("MERGIFY_TEST_HELPER_Y", Some(""), || {
            var_non_empty("MERGIFY_TEST_HELPER_Y")
        });
        assert_eq!(got, None);
    }

    #[test]
    fn returns_none_when_unset() {
        let got = testing::with_var("MERGIFY_TEST_HELPER_Z", None::<&str>, || {
            var_non_empty("MERGIFY_TEST_HELPER_Z")
        });
        assert_eq!(got, None);
    }

    #[test]
    fn var_os_non_empty_treats_empty_as_unset() {
        // Same empty-as-unset rule as `var_non_empty`, on the path
        // that keeps the value an `OsString`.
        testing::with_vars(
            [("MERGIFY_TEST_A", Some("x")), ("MERGIFY_TEST_B", Some(""))],
            || {
                assert_eq!(var_os_non_empty("MERGIFY_TEST_A"), Some("x".into()));
                assert_eq!(var_os_non_empty("MERGIFY_TEST_B"), None);
                assert_eq!(var_os_non_empty("MERGIFY_TEST_C"), None);
            },
        );
    }

    #[test]
    fn with_no_vars_is_the_empty_environment() {
        assert_eq!(testing::with_no_vars(|| var("PATH")), None);
    }

    #[test]
    fn with_no_vars_does_not_inherit_an_enclosing_overlay() {
        // The trap this exists to avoid: `with_vars` layers onto the
        // enclosing overlay, so an *empty* `with_vars` nested inside
        // one would keep everything and scrub nothing.
        testing::with_var("MERGIFY_TEST_A", Some("outer"), || {
            testing::with_no_vars(|| assert_eq!(var("MERGIFY_TEST_A"), None));
            assert_eq!(var("MERGIFY_TEST_A").as_deref(), Some("outer"));
        });
    }

    #[test]
    fn overlay_hides_the_process_environment() {
        // The property the old `temp_env` scrub lists existed to
        // fake: a variable the test did not list is unset, whatever
        // the host exports. `PATH` is the likeliest name to be
        // exported, but nothing guarantees it — a process started
        // with a cleared environment has none — so read the host's
        // value rather than requiring one.
        let host_path = var("PATH");
        let got = testing::with_var("MERGIFY_TEST_HELPER_X", Some("hello"), || var("PATH"));
        assert_eq!(got, None, "an installed overlay is the whole environment");
        assert_eq!(
            var("PATH"),
            host_path,
            "outside an overlay the real environment is visible again"
        );
    }

    #[test]
    fn nested_overlays_layer_and_unwind() {
        testing::with_vars(
            [
                ("MERGIFY_TEST_A", Some("outer")),
                ("MERGIFY_TEST_B", Some("b")),
            ],
            || {
                testing::with_vars(
                    [("MERGIFY_TEST_A", Some("inner")), ("MERGIFY_TEST_B", None)],
                    || {
                        assert_eq!(var("MERGIFY_TEST_A").as_deref(), Some("inner"));
                        assert_eq!(var("MERGIFY_TEST_B"), None);
                    },
                );
                assert_eq!(var("MERGIFY_TEST_A").as_deref(), Some("outer"));
                assert_eq!(var("MERGIFY_TEST_B").as_deref(), Some("b"));
            },
        );
        assert_eq!(var("MERGIFY_TEST_A"), None);
    }

    #[test]
    fn overlay_is_restored_after_a_panic() {
        let panicked = std::panic::catch_unwind(|| {
            testing::with_var("MERGIFY_TEST_A", Some("x"), || panic!("boom"));
        });
        assert!(panicked.is_err());
        // The overlay's own variable, not a host one: `PATH` reads as
        // unset both under a leaked overlay and on a host that does
        // not export it, so it cannot tell those two apart.
        assert_eq!(
            var("MERGIFY_TEST_A"),
            None,
            "the overlay outlived the panic that unwound through it"
        );
    }

    #[tokio::test]
    async fn overlay_spans_await_points() {
        let got = testing::with_vars_async([("MERGIFY_TEST_A", Some("x"))], async {
            tokio::task::yield_now().await;
            var("MERGIFY_TEST_A")
        })
        .await;
        assert_eq!(got.as_deref(), Some("x"));
    }
}
