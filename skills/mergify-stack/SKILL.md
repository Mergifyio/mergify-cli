---
name: mergify-stack
description: Use Mergify stacks for git push, commit, branch, and PR creation. ALWAYS use this skill when pushing code, creating commits, creating branches, or creating PRs. Triggers on push, commit, branch, PR, pull request, stack, git.
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
- **Mid-stack fixes**: Use `git rebase -i` to edit the specific commit, amend it, continue rebase, then `mergify stack push`
- **Commit titles**: Follow [Conventional Commits](https://www.conventionalcommits.org/) (e.g., `feat:`, `fix:`, `docs:`)
- **PR title & body**: `mergify stack` copies the commit message title to the PR title and the commit message body to the PR body — so write commit messages as if they were PR descriptions. **Everything that should appear in the PR (ticket references, context, test plans) MUST go in the commit message.**
- **Ticket references**: Include ticket/issue references (e.g., `MRGFY-1234`, `Fixes #123`) in the commit message body, not added separately to the PR.
- **PR lifecycle is fully managed by `mergify stack`**: NEVER edit PR titles, bodies, or labels with `gh pr edit` or the GitHub MCP — they will be overwritten on the next push. NEVER close or merge PRs manually — `mergify stack` handles the entire PR lifecycle (creation, updates, and cleanup).
- **Draft PRs**: NEVER mark a PR as ready-for-review — all PRs stay as drafts. The user will manually move them out of draft after reviewing.
- **Each commit must pass CI independently**: Every commit in a stack becomes its own PR. Each PR runs CI separately, so every commit must be self-contained — it must compile, pass linters, and pass tests on its own without depending on later commits in the stack. When formatting or linting fixes are needed, they must be included in the commit that introduced the issue, not deferred to a later commit.

## Common Mistakes

| Wrong | Right | Why |
|-------|-------|-----|
| `git push` | `mergify stack push` | Git push bypasses stack management and breaks PR relationships |
| New commit to fix lint/typo | `git commit --amend` (HEAD) or `git commit --fixup <SHA>` + `git rebase --autosquash` (mid-stack) | Each commit = a PR; fix commits create unwanted extra PRs |
| `gh pr edit --title "..."` | Edit the commit message, then `mergify stack push` | PR title/body are overwritten from commit messages on every push |
| `gh pr merge` or `gh pr close` | PR lifecycle is fully managed — do nothing | PR lifecycle is fully managed by the stack tool |
| `git commit` on `main` | `mergify stack new <name>` first | `mergify stack push` will fail on the default branch |
| Deferring lint fixes to a later commit | Include the fix in the commit that caused it | Each commit runs CI independently; later commits won't save earlier ones |

## Commands

```bash
mergify stack new NAME       # Create a new stack/branch for new work
mergify stack push           # Push and create/update PRs
mergify stack sync           # Fetch trunk, remove merged commits, rebase
mergify stack list           # Show commit <-> PR mapping for current stack
mergify stack list --json    # Same, but machine-readable JSON output
```

Use `mergify stack sync` to bring your stack up to date. It fetches the latest trunk, detects which PRs have been merged, removes those commits from your local branch, and rebases the remaining commits. Run this before starting new work on an existing stack.

Use `mergify stack list` to see which commits have been pushed, which PRs they map to, and whether the stack is up to date with the remote. This is the go-to command to understand the current state of a stack. Use `--json` when you need to parse the output programmatically.

## CRITICAL: Check Branch Before ANY Commit

**BEFORE staging or committing anything**, always check the current branch and assess stack state:

```bash
git branch --show-current
mergify stack list
```

- If you're on `main` (or the repo's default branch): you **MUST** create a feature branch first
- **NEVER commit directly on `main`** — `mergify stack push` will fail
- This check must happen before `git add`, not after `git commit`

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
