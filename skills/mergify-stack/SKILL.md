---
name: mergify-stack
description: Use Mergify stacks for git push, commit, branch, and PR creation. ALWAYS use this skill when pushing code, creating commits, creating branches, or creating PRs. Triggers on push, commit, branch, PR, pull request, stack, stacked, git, rebase, checkout, reorder, move, sync, amend, note, revision history.
---

# Mergify Stack Workflow

## Stack Philosophy

A branch is a stack. Keep stacks short and focused:
- A stack should only contain commits that **depend on each other**
- Rationale: longer stacks take longer to merge

**Proactive stack management:**
- If an existing stack can be split into independent stacks, offer to do so
- When asked to do something new: if it can be done on a separate branch, either do so or ask if in doubt
- Default to creating a new branch for unrelated changes

## Core Conventions

- **Push**: Use `mergify stack push` (never `git push`)
- **Fixes**: Use `git commit --amend` (never create new commits to fix issues)
- **Amend notes**: When amending a commit that already has a PR (i.e. has been pushed), attach a `mergify stack note` BEFORE `mergify stack push` to record *why* the commit was amended. The note appears in the PR's "Revision history" comment and JSON marker, so reviewers can see the reason without diffing. It rides along with the commit through amends and rebases, so attaching it once is enough. See [Amend Notes](#amend-notes).
- **Target by Change-Id, not SHA**: Address a commit by its `Change-Id` trailer — that is what maps a commit to its PR, and it survives every rebase, while SHAs change on every rebasing push. Both forms are accepted, and the difference when a SHA has gone stale matters: `stack edit` (like `drop`/`squash`/`fixup`/`move`/`reword`/`reorder`) matches the prefix against the commits *in the stack*, so a stale SHA fails loudly with `no commit found matching SHA prefix`; `mergify stack note` resolves any git revision without checking stack membership, so a stale SHA silently attaches the note to a commit no push will ever read.
- **Mid-stack fixes**: Stash any local changes first (`git stash -u`), then use `mergify stack edit <Change-Id>` to pause the rebase at the target commit. `stack edit` pauses *on* the commit with a clean tree and git tells you to amend — that is correct: `git commit --amend --no-edit`, then `git rebase --continue`, then `mergify stack push`, then `git stash pop`. `--no-edit` keeps the message verbatim, so the existing `Change-Id` trailer (which maps the commit to its PR) is retained — the `commit-msg` hook only *inserts* a `Change-Id` when one is missing, so it won't rewrite yours. Amend ONLY at this edit pause — a rebase *conflict* pause is the opposite trap (see [Conflict Resolution](#conflict-resolution)). Non-interactive — never use `git rebase -i` for this. (Calling `mergify stack edit` with no argument falls back to a fully interactive `git rebase -i` and will hang in agent contexts — always pass a `Change-Id`.)
- **Reordering**: Stash any local changes first (`git stash -u`), then use `mergify stack reorder` (list all commits in desired order) or `mergify stack move` (move a single commit) instead of manual `git rebase -i` — non-interactive and avoids `GIT_SEQUENCE_EDITOR` quoting issues
- **Fixup**: Stash any local changes first (`git stash -u`), then use `mergify stack fixup <SHA>...` to fold a commit into its parent (drops the listed commit's message). Non-interactive — never use `git rebase -i` for this.
- **Squash**: Stash any local changes first (`git stash -u`), then use `mergify stack squash SRC... into TARGET [-m "msg"]` to combine multiple commits into one, with an optional custom message. Non-interactive — never use `git rebase -i` for this.
- **Reword**: Stash any local changes first (`git stash -u`), then use `mergify stack reword <SHA> -m "new message"` to change a commit's message in place. Non-interactive when `-m` is given — never use `git rebase -i` for this.
- **Drop**: Stash any local changes first (`git stash -u`), then use `mergify stack drop <SHA>...` to remove commits from the stack. Non-interactive — never use `git rebase -i` for this.
- **Commit titles**: Follow [Conventional Commits](https://www.conventionalcommits.org/) (e.g., `feat:`, `fix:`, `docs:`)
- **PR title & body**: `mergify stack` copies the commit message title to the PR title and the commit message body to the PR body — so write commit messages as if they were PR descriptions. **Everything that should appear in the PR (ticket references, context, test plans) MUST go in the commit message.** Unless `mergify-cli.stack-keep-pr-title-body` is set, every push re-syncs each PR's title and body from its commit message; and because a reword or amend at the *bottom* of a stack rebases everything above it, that push re-syncs *every* PR's body, not only the one you changed. This is why hand-edits via `gh pr edit --body` never stick (see the lifecycle rule below) — put the content in the commit message instead. If you truly must hand-edit a body, do it last, after the final push.
- **Ticket references**: Include ticket/issue references (e.g., `MRGFY-1234`, `Fixes #123`) in the commit message body, not added separately to the PR.
- **PR lifecycle is fully managed by `mergify stack`**: NEVER edit PR titles, bodies, or labels with `gh pr edit` or the GitHub MCP — they will be overwritten on the next push. NEVER close or merge PRs manually — `mergify stack` handles the entire PR lifecycle (creation, updates, and cleanup).
- **Draft PRs**: NEVER mark a PR as ready-for-review — all PRs stay as drafts; the user moves them out of draft after reviewing. Be aware this has a CI consequence: a stacked **draft** PR may get **no CI at all** (some CI setups gate their pipeline's entry job on the PR being non-draft), so the upper PRs can show a wall of `skipping` — that is "never ran", not "green". CI only starts once the PR is readied, which is the user's call — so don't read a skipped draft as passing. See [Reading CI status](#reading-ci-status).
- **Each commit must pass CI independently**: Every commit in a stack becomes its own PR. Each PR runs CI separately, so every commit must be self-contained — it must compile, pass linters, and pass tests on its own without depending on later commits in the stack. When formatting or linting fixes are needed, they must be included in the commit that introduced the issue, not deferred to a later commit.

## Common Mistakes

| Wrong | Right | Why |
|-------|-------|-----|
| `git push` | `mergify stack push` | Git push bypasses stack management and breaks PR relationships |
| New commit to fix lint/typo | `git commit --amend` (HEAD) or `git commit --fixup <SHA>` + `git rebase --autosquash` (mid-stack) | Each commit = a PR; fix commits create unwanted extra PRs |
| `gh pr edit --title "..."` | Edit the commit message, then `mergify stack push` | PR title/body are overwritten from commit messages on every push |
| `gh pr merge` or `gh pr close` | PR lifecycle is fully managed — do nothing | PR lifecycle is fully managed by the stack tool |
| `git commit` on `main` | `mergify stack new <name>` first | `mergify stack push` will fail on the default branch |
| `git rebase -i` to fixup a commit | `mergify stack fixup <SHA>` | Non-interactive — works inside LLM/agent sessions; no editor spawned |
| `git rebase -i` to squash commits | `mergify stack squash A B into X [-m "..."]` | Non-interactive — works inside LLM/agent sessions; no editor spawned |
| `git rebase -i` to change a commit message | `mergify stack reword <SHA> -m "..."` | Non-interactive — works inside LLM/agent sessions; no editor spawned |
| `git rebase -i` to amend a mid-stack commit | `mergify stack edit <Change-Id>` then `git commit --amend --no-edit` then `git rebase --continue` | Non-interactive — pauses the rebase at the target commit; `--no-edit` keeps the message (and its `Change-Id`) verbatim without spawning an editor |
| `git rebase -i` to drop a commit | `mergify stack drop <SHA>...` | Non-interactive — works inside LLM/agent sessions; no editor spawned |
| `GIT_SEQUENCE_EDITOR='sed -i ...' git rebase -i` (any variant) | One of `mergify stack {edit,fixup,squash,reorder,move}` | Hand-rolled sequence-editor scripts are brittle; there is already a non-interactive command for every common rewrite |
| Deferring lint fixes to a later commit | Include the fix in the commit that caused it | Each commit runs CI independently; later commits won't save earlier ones |
| Rebase/reorder/checkout/sync with dirty worktree | `git stash -u` first, then `git stash pop` after | Uncommitted changes are lost or cause conflicts during these operations |
| Amending a pushed commit with no explanation | `mergify stack note -m "why"` before `mergify stack push` | The reason is recorded in the PR's Revision history table and JSON marker, so reviewers don't need to diff to understand the change |
| `git commit --amend` at a rebase *conflict* pause | `git add <files> && git rebase --continue` | The conflicting pick hasn't produced a commit yet; amending rewrites the last-applied commit and re-maps your work to the wrong PR's `Change-Id` |

## Commands

```bash
mergify stack new NAME       # Create a new stack/branch for new work
mergify stack push           # Push and create/update PRs (rebases onto latest trunk first, unless PRs are approved)
mergify stack checkout NAME  # Checkout an existing stack from GitHub (e.g. someone else's)
mergify stack sync           # Fetch trunk, remove merged commits, rebase
mergify stack list           # Show commit <-> PR mapping for current stack
mergify stack list --json    # Same, but machine-readable JSON output
mergify stack reorder C A B  # Reorder all commits (pass SHA or Change-Id prefixes)
mergify stack move X first   # Move commit X to the top of the stack
mergify stack move X last    # Move commit X to the bottom of the stack
mergify stack move X before Y  # Move commit X before commit Y
mergify stack move X after Y   # Move commit X after commit Y
mergify stack fixup X              # Fold commit X into its parent (drops X's message)
mergify stack fixup X Y Z          # Fold each into its parent (multi-fixup)
mergify stack squash X into Y      # Reorder X adjacent to Y, fold X into Y (keeps Y's message)
mergify stack squash X Y into Z -m "msg"  # Fold X Y into Z with a custom message
mergify stack reword X -m "msg"    # Change commit X's message non-interactively
mergify stack reword X             # Change commit X's message via $GIT_EDITOR (TTY only)
mergify stack edit X               # Pause the rebase at X so you can `git commit --amend` it (X is required; no-arg form is interactive)
mergify stack drop X               # Drop commit X from the stack
mergify stack drop X Y Z           # Drop multiple commits in one rebase
mergify stack note -m "why"        # Attach an amend reason to HEAD (shown in PR revision history)
mergify stack note <SHA-or-Change-Id-prefix> -m "why"  # Attach to a specific commit in the stack
mergify stack note --append -m "more"                  # Append to an existing note
mergify stack note --remove                            # Remove the note from a commit
```

Use `mergify stack checkout NAME` to check out a stack that exists on GitHub (e.g. a colleague's stack). NAME is the remote branch name of the stack. It fetches all stacked PRs, creates a local branch, and sets up tracking. Use `--branch` to override the local branch name.

Use `mergify stack sync` to bring your stack up to date. It fetches the latest trunk, detects which PRs have been merged, removes those commits from your local branch, and rebases the remaining commits. Run this before starting new work on an existing stack.

Use `mergify stack list` to see which commits have been pushed, which PRs they map to, and whether the stack is up to date with the remote. It also shows CI status, review status, and merge conflicts for each PR. Use `--verbose` for detailed check names and reviewer names. Use `--json` when you need to parse the output programmatically — it includes full CI check details and review data.

## Amend Notes

`mergify stack note` records *why* a commit was amended. The note travels with the stack:

- The reason is stored locally under `refs/notes/mergify/stack` against the commit SHA.
- On `mergify stack push`, the reason is consumed into the change's revision history; the note on the pushed head commit is replaced by the **full revision history** (human digest + the `<!-- mergify-revision-data: {...} -->` JSON marker). Git notes — not the PR comment — are the machine-readable source of truth; the PR's "Revision history" comment is rendered from them.
- At merge time, Mergify copies the head commit's history note onto the merge/squash commit, so `git log --notes=mergify/stack` on the base branch shows why each change was revised.

**When to attach a note** — any time you amend or rewrite a commit that already has a PR open (i.e. it has been pushed at least once). The note answers "why is this revision different?" so the reviewer doesn't have to diff old vs new SHAs to find out.

**Workflow** — attach the note BEFORE `mergify stack push`:

```bash
# Edit HEAD, then:
git commit --amend
mergify stack note -m "address review: rename foo() to bar()"
mergify stack push

# Or for a mid-stack commit (after the rebase that amended it):
mergify stack note <Change-Id> -m "fix lint reported in CI"
mergify stack push
```

A note is per-commit, not per-revision. It is stored against the commit SHA, but git carries it onto the rewritten commit through an amend or a rebase (`mergify stack setup` configures `notes.rewriteRef` for this), so the reason you attach survives the rebase `stack push` does and lands in the revision history. Use `--append` only when the current target commit already has a note and you want to add another reason; use `--remove` to clear it. Notes on commits that haven't changed since the last push are preserved but won't add a new revision row.

## Reading CI status

- Read a PR's status with **`gh pr checks <pr>`** and trust its exit code: `0` = all checks passed, `8` = a check is still pending. Any other non-zero code means "not green" but not necessarily a CI failure: `1` usually means a check failed, yet it is also `gh`'s generic error code (unknown PR, network error), and auth failures exit `4` — so read the output before calling any of them a failure. (`gh pr checks` considers *all* checks by default; add `--required` to consider only required ones.)
- The `mergify stack list` CI column can go **stale** right after a rapid re-push — give CI a moment to register the new head, or confirm with `gh pr checks`.
- `statusCheckRollup` (in `gh pr view --json` / the GitHub API) keeps `CANCELLED` entries from superseded runs: each re-push cancels the previous commit's in-flight CI, so those cancellations are **history, not failures**.
- A stacked **draft** PR may show no checks at all — see the [Draft PRs](#core-conventions) convention for why a wall of `skipping` is "never ran", not "green".

## CRITICAL: Check Branch Before ANY Commit

**BEFORE staging or committing anything**, always check the current branch and assess stack state:

```bash
git branch --show-current
mergify stack list
```

- If you're on `main` (or the repo's default branch): you **MUST** create a feature branch first
- **NEVER commit directly on `main`** — `mergify stack push` will fail
- This check must happen before `git add`, not after `git commit`

## CRITICAL: Stash Local Changes Before Worktree-Modifying Operations

**BEFORE running any operation that rewrites history or switches branches**, check for uncommitted changes and stash them:

```bash
git status --short          # Check for uncommitted changes
git stash -u                # Stash tracked + untracked changes if any
```

**Operations that require this check:**
- `mergify stack edit <commit>` (mid-stack fixes)
- `mergify stack reorder` / `mergify stack move`
- `mergify stack fixup`
- `mergify stack squash`
- `mergify stack reword`
- `mergify stack drop`
- `mergify stack checkout`
- `mergify stack sync`
- `mergify stack new` (switches to new branch)
- `git checkout <branch>`

**After the operation completes**, restore the stashed changes:

```bash
git stash pop
```

If you skip this step, uncommitted work will be **silently lost** or cause rebase conflicts.

## Starting New Work

When asked to start a new piece of work, create a new feature, or work on something unrelated to the current stack:

1. **Check current branch**: `git branch --show-current`
2. **Create a new stack**: `mergify stack new <branch-name>`
   - Use descriptive branch names following the pattern: `type/short-description` (e.g., `feat/add-login`, `fix/memory-leak`)
3. **Make commits** following conventional commits
4. **Push**: `mergify stack push`

## Adding to Existing Stack

When continuing work on an existing feature branch:

1. **Check current branch**: `git branch --show-current`
   - If on the right branch: proceed with commits
   - If on `main`: switch to the feature branch first with `git checkout <branch>` or create a new stack

## Conflict Resolution

When a rebase causes conflicts (during `git rebase -i` or `mergify stack push`):

1. Resolve conflicts in your editor
2. Stage resolved files with `git add`
3. Continue with `git rebase --continue`

To abort instead: `git rebase --abort`

After resolving, run `mergify stack push` to sync the updated stack.

**NEVER `git commit --amend` at a conflict pause.** At a conflict the pick being replayed has **not** produced a commit yet, and HEAD still points at the last commit git already applied — the trunk tip if the *bottom* commit conflicted, otherwise a previously-replayed commit of your own stack. Either way it is **not** the commit you're resolving, so `git commit --amend` rewrites that already-applied commit — silently stamping your work with its title, author, and `Change-Id` and re-mapping it to the WRONG PR. Resolve a conflict with `git add <files> && git rebase --continue`, nothing more. Amend only ever belongs at a `mergify stack edit` pause (see **Mid-stack fixes**), where the tree is clean and git itself tells you to amend. If you already amended mid-conflict, compare HEAD's message against the commit you *meant* to resolve; if it doesn't match, the commit needs rebuilding from the correct tree. The `commit-msg` hook installed by `mergify stack setup` refuses this amend outright, so an up-to-date checkout stops you before the damage — but `--no-verify` walks straight past it.
