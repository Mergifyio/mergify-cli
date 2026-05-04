# Agent Instructions

## CLI Output Style

Use compact Unicode symbols for terminal output, not emoji. This keeps new and
updated terminal output consistent and avoids rendering issues across terminals.

**Preferred symbols:**
- `✓` success, approved, merged
- `✗` failure, conflict, changes requested
- `●` active, pending, in progress
- `○` inactive, skipped, none
- `—` unknown, not applicable

**Avoid in terminal output:** `✅`, `❌`, `🟢`, `🔴`, `📝`, `⚠️`, `🔄`, `📦`, `🤖`, `🔗`
and other emoji. They can render at inconsistent widths across terminals and
break column alignment.

**Exceptions — emoji are acceptable in:**
- Markdown output destined for GitHub (PR comments, CI summaries)
- CI log output (`ci junit-process`, `ci scopes`) — these are read in CI
  runners (GitHub Actions, CircleCI, etc.) where emoji render consistently
  and improve log scanability

## Error Messages

Use `console_error(message)` from `mergify_cli` for all user-facing error
messages. It prints `error: {message}` in red with `markup=False` (safe for
user data containing `[`/`]`). Keep the message
lowercase after `error:` (the function adds the prefix).

```python
from mergify_cli import console_error

console_error("commit not found")          # -> "error: commit not found"
console_error(f"failed to fetch: {e}")     # -> "error: failed to fetch: ..."
```

For follow-up hints after an error, use plain `console.print()`:
```python
console_error("rebase failed — there may be conflicts")
console.print("Resolve conflicts then run: git rebase --continue")
```

Do **not** use `console.print(..., style="red")` or `[red]...[/]` markup
for error messages — use `console_error()` instead.

## Success Messages

End success messages with a period:
```python
console.print("[green]Freeze created successfully.[/]")
console.print("Stack reordered successfully.", style="green")
```

## JSON Output

Commands with `--json` flags must write JSON to **stdout** using
`click.echo()`, not `console.print()` (which may add Rich formatting).
This ensures clean output pipeable to `jq` and other tools.

```python
if output_json:
    click.echo(json.dumps(data, indent=2))
```

## Command Groups

All top-level command groups must use `DYMGroup` (from `mergify_cli.dym`)
with `invoke_without_command=True` for consistent "did you mean?"
suggestions. Add an explicit `click.echo(ctx.get_help())` in the group
callback when `ctx.invoked_subcommand is None` for help display.

## Rust Port Workflow

The CLI is being ported from Python to Rust incrementally. The shipped
binary is `mergify` (built from `crates/mergify-cli`); commands not yet
ported fall through to a Python shim implemented by the
`crates/mergify-py-shim` crate, which invokes `python -m mergify_cli`
on the bundled Python source. Native Rust commands are dispatched
directly. Drift between the two implementations is prevented
structurally: when porting a command, the Python implementation MUST
be deleted in the same PR that adds the Rust implementation. There is
no period where both copies coexist.

A single PR therefore contains:

1. The Rust implementation (in the relevant `crates/*` crate) plus tests.
2. Removal of the Python implementation file(s) and their tests.
3. Any wiring updates (click registration, shim allow-list, etc.).

Reviewers should reject PRs that port a command without removing the
Python copy. Removing the Python copy without a Rust replacement is
fine when the command is being deprecated/dropped from the CLI — the
rule is "no two live copies of the same command", not "every Python
copy must be replaced".

## Documentation

When adding or changing a CLI feature, always update the documentation:

1. **README.md** — keep the Features and Usage sections current with all
   top-level commands and a brief description of what each does.
2. **Skills** (`skills/` directory) — if the feature is something an AI agent
   should know how to use, ensure a corresponding SKILL.md exists and covers it.
   Update existing skills when command signatures or workflows change.

Documentation updates should be part of the same commit/PR as the feature
change, not a follow-up.
