# Agent Instructions

`mergify` is a native Rust CLI — a Cargo workspace under `crates/`, with the
binary built from `crates/mergify-cli` (`[[bin]] name = "mergify"`). There is no
Python: the port is complete and every command runs in-process. If you find a
doc, comment, or rule mentioning `console_error`, `click`, `DYMGroup`,
`mergify_cli.dym`, `crates/mergify-py-shim`, or a "Python shim", it is stale —
fix it.

The bar is ruff/uv-grade Rust: typed errors, no panics in library code, clippy
pedantic clean, snapshot-tested output. Concrete rules follow.

## Workspace layout

- `mergify-core` — shared foundations: `CliError`/`ExitCode`, the HTTP `Client`,
  `auth`, `Output`, `env`.
- `mergify-tui` — terminal primitives: `Theme` (color), glyphs, relative time.
  Stays dependency-light and **clap-free**.
- `mergify-cli` — the binary: clap tree, dispatch, `run_native`, `self_update`,
  `cli_schema`.
- `mergify-stack` / `mergify-ci` / `mergify-queue` / `mergify-freeze` /
  `mergify-config` — one crate per command group.
- `mergify-test-support` — shared test scaffolding (not published).

## Error handling

Library and command code **never prints errors and never calls `process::exit`**.
Return `Result<_, mergify_core::CliError>`. `main()` is the single error sink: it
writes `mergify: {err}` to stderr, walks the `source()` chain printing each cause
as a `caused by:` line, and exits with `err.exit_code()`.

`CliError` is a typed enum; each variant maps to a stable `ExitCode`
(`GitHubApi`→5, `MergifyApi`→6, `Configuration`→7, … see `exit_code.rs`). Pick the
variant whose exit code matches the failure class. Never add a `String` catch-all
for a new failure category that deserves its own code — add a variant.

```rust
// GOOD — typed variant, lowercase message, no prefix (the sink adds "mergify:")
return Err(CliError::StackNotFound(format!("branch {name} not found")));

// BAD — printing + exiting inside a command
eprintln!("error: branch not found");
std::process::exit(1);

// BAD — capitalized / pre-prefixed (the sink double-prefixes it)
return Err(CliError::Generic("Error: branch not found".into()));
```

**Preserve causes — don't flatten them into a string.** Prefer keeping a typed
cause over `format!("...: {e}")`:

- self-describing typed error → `CliError::Source` (or `?` it through the
  generated `From<Box<dyn Error + Send + Sync>>`), which keeps it transparently.
- "doing X failed because of Y" → `CliError::wrap(context, e)`, which shows the
  context as the headline and `e` as a `caused by:` line.

```rust
// GOOD — context + preserved cause, prints a `caused by:` line
let exe = std::env::current_exe().map_err(|e| CliError::wrap("locate binary", e))?;

// AVOID — flattens the cause; reach for Generic only when there's no typed cause
let exe = std::env::current_exe()
    .map_err(|e| CliError::Generic(format!("locate binary: {e}")))?;
```

**No `anyhow` / `eyre` / `miette`.** Deliberate house stance: libraries return the
typed `CliError`; the binary prints it (and the chain). If you need a richer
rendered report, extend `CliError` + the `main()` sink in the PR — don't add
`anyhow`.

**API calls go through `mergify_core::http::Client`** with the right `ApiFlavor`,
which assigns the correct exit code and gets retries (5xx, 429/`Retry-After`
rate-limits), timeouts, and the `User-Agent` for free. **Never `use reqwest`
directly from a command crate** (the one exception is `self_update`, documented in
place: it does unauthenticated cross-host binary downloads the JSON client can't
model).

## No panics in library / command paths

A panic is a crash with no exit-code contract. In non-test code do not
`.unwrap()`, `.expect()`, `panic!`, `unreachable!`, or `todo!` on anything derived
from user input, the network, the filesystem, or git output. Return a `CliError`.

```rust
// BAD
let n = pull["number"].as_u64().unwrap();
// GOOD
let n = pull["number"].as_u64()
    .ok_or_else(|| CliError::Generic("pull payload missing `number`".into()))?;
```

**Allowed exceptions** (must be provably infallible; add a one-line comment why):
`Regex::new` on a literal behind `LazyLock`; `write!`/`writeln!` into a `String`;
a mandatory regex capture group after the overall match succeeded; an
`unreachable!` arm for a case handled earlier in the same function (e.g. the
pre-runtime introspection commands). Prefer encoding an invariant in the type so
the `expect` disappears; a stringly `match … => panic!("unknown X")` is **not**
allowed — use an exhaustive `enum` match.

## clap conventions

- Every command group is a derive subcommand enum: `Group(GroupArgs)` where
  `GroupArgs` holds `#[command(subcommand)] command: GroupSubcommand`. Do **not**
  hand-roll a `trailing_var_arg` catch-all and re-parse argv — clap gives correct
  usage, help, did-you-mean, and global-flag inheritance for free. (`stack` is a
  normal subcommand group; if you see a "shim"/`ShimmedArgs`, it's a regression.)
- Register every new `(group, subcommand)` pair in `NATIVE_COMMANDS` in
  `main.rs` — it's the single source of truth behind `--list-native-commands`,
  and the schema golden asserts the `stack` set matches it. Adding a command
  without updating it makes the test fail.
- Global flags (`--debug`, `-v/--verbose`, `--color`) are `#[arg(global = true)]`
  so they work on every subcommand.
- Pure-introspection commands (`completions`, `_internal dump-cli-schema`,
  `_internal man`) are handled before the tokio runtime starts.

## stdout / stderr / `--json` / color

- **Machine output (JSON) → stdout. Human chatter, progress, prompts, errors,
  and logs → stderr.** Piping stdout into `jq` must stay clean.
- Commands emit results through `&mut dyn mergify_core::output::Output`, not
  `println!`. Call `emit` / `emit_json_value` once.
- **Color goes through `mergify_tui::theme`**, never hardcoded SGR escapes. The
  theme honors `--color <auto|always|never>` (resolved once via
  `set_color_choice`), then `NO_COLOR`, then `FORCE_COLOR`/`CLICOLOR_FORCE`, then
  the TTY. Cursor-movement / erase escapes in the progress renderer are not color
  and are fine.

```rust
// GOOD
writeln!(w, "{}created.{}", theme.green.render(), theme.green.render_reset())?;
// BAD
writeln!(w, "\x1b[32mcreated.\x1b[0m")?;
```

## Terminal output symbols

Compact, single-width Unicode only — never emoji (they render at inconsistent
widths and break column alignment).

| Symbol | Meaning |
|---|---|
| `✓` | success, approved, merged |
| `✗` | failure, conflict, changes requested |
| `●` | active, pending, in progress |
| `○` | inactive, skipped, none |
| `—` | unknown, not applicable |

**Forbidden in terminal output:** `✅ ❌ 🟢 🔴 ⏰ ⏳ ⚠️ 🔄 📦` and other emoji.
**Emoji are allowed only in** Markdown destined for GitHub (PR comments, CI
summaries) and CI-log output (`ci junit-process`, `ci scopes`) read in CI runners.

Success messages end with a period ("Stack reordered successfully.") in the
theme's green, applied inside the human-render closure.

## Logging

Structured logs use `tracing`, emitted to **stderr** by the subscriber `main()`
installs. `-v`/`-vv`/`-vvv` map to info/debug/trace; `--debug` floors at debug;
`RUST_LOG` overrides. Only our own crates are raised, so dep noise stays quiet.
Instrument network/process boundaries at `debug`; **never log the bearer token**
or any auth header.

## HTTP / API rules

- Match the endpoint's real response shape. Empty-body success → use
  `post_no_response` / `delete_if_exists`, never `let _: serde_json::Value =
  client.post(...)` (that fails to deserialize an empty body).
- Fields the server may omit/null are `Option<T>` with `#[serde(default)]`.
- Build one `reqwest::Client` per command run and reuse it across same-host calls
  (it owns the connection pool); don't rebuild per request.

## Testing

A change is not done without a test. Use the highest-fidelity tool per layer.

- **Snapshot the output that *is* the product** with `insta`. The CLI schema has
  a golden (`cli_schema_golden`, version redacted) that catches any flag /
  subcommand / help / value-hint drift; accept intentional changes with
  `cargo insta review`. Add insta goldens for colorized human renders (a fixed
  `now` seam and `cfg!(test)` color-off make them deterministic). Keep exact
  `assert_eq!` on `serde_json::Value` for `--json` payloads — stronger than a
  string snapshot.
- **HTTP commands: wiremock with payload + call-count fidelity.** Assert
  `body_json(...)` and `.expect(N)`, not just "didn't crash". Stub an empty-body
  endpoint with `ResponseTemplate::new(200)` — never `set_body_json(json!({}))`,
  which masks the empty-body bug the rule exists to catch.
- Pin exact exit codes for the `CliError` contract; add a regression test for
  every fixed failure mode (exit code, message, missing-field tolerance).
- Use `temp_env::with_var` for env-dependent tests — never the unsound
  process-global `std::env::set_var` (`unsafe_code = "forbid"` bans it anyway).

## Dependencies

Shared third-party deps live in the root `[workspace.dependencies]` table; each
crate references them as `dep = { workspace = true }` (add extra features with
`features = [...]`, which are additive). When adding or bumping a shared dep, edit
the **root table**, not the crate — that's the single source of truth.

- TLS is `rustls` everywhere (`default-features = false, features = ["rustls"]`).
  Never pull in `native-tls`/`openssl`.
- Internal crates are `{ path = "../…" }` path deps.
- `cargo-deny` gates advisories, licenses, bans, and sources (`deny.toml`); a new
  dep with a non-allowed license fails CI — add the SPDX id (with the crate that
  needs it) rather than loosening the policy.

## clippy / fmt / MSRV bar

- CI runs `cargo fmt --all --check` and
  `cargo clippy --workspace --all-targets --all-features --locked -- -D warnings`.
  **Warnings fail the build.** Run both before pushing.
- Lints are `clippy::all` + `clippy::pedantic` (warn) with a short allow-list. Do
  not add `#[allow(...)]` without a one-line reason, and don't grow the workspace
  allow-list without justifying it in the PR.
- MSRV is `rust-version = "1.88"`, gated by a CI job that compiles on exactly that
  toolchain. Don't use features stabilized after it unless you bump
  `rust-version` (and say so in the commit) — the job will catch it.

## Documentation

Doc updates ship in the **same** commit/PR as the change, never a follow-up.

1. **README.md** — keep Commands/Usage current with every top-level command
   (including `self-update` and `completions`).
2. **Skills** (`skills/`) — update the *matching* skill (don't create a
   duplicate). Directory names aren't 1:1 with command groups:

   | command group | skill dir |
   |---|---|
   | `stack` | `mergify-stack` |
   | `ci`, `tests` | `mergify-ci` |
   | `config` | `mergify-config` |
   | `queue` | `mergify-merge-queue` |
   | `freeze` | `mergify-merge-protections` |

3. **Crate `//!` module docs** — keep the purpose/invariant header accurate when
   you change a module's contract (the `Output` stdout-purity invariant, the
   `CliError`→`ExitCode` mapping, the HTTP retry policy).

## Commits & PRs

- Conventional-commit subjects scoped by area: `feat(stack):`, `fix(ci):`,
  `chore(deps):`, `feat(cli):`. Imperative, no trailing period. The commit body
  becomes the PR body — put ticket refs and context there.
- Never commit on `main`; never `git push` — use `mergify stack push`. Each
  commit becomes its own PR and must pass CI **independently** (compile, clippy
  `-D warnings`, tests). Fold lint/fmt fixes into the commit that caused them.
- Small in-flight polish → `git commit --amend` (+ `mergify stack note -m "why"`
  before re-pushing), not a pile of fixup commits.
