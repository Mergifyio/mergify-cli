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

**Exception:** Emoji are acceptable in markdown output destined for GitHub
(PR comments, CI summaries) where they render consistently.

## Documentation

When adding or changing a CLI feature, always update the documentation:

1. **README.md** — keep the Features and Usage sections current with all
   top-level commands and a brief description of what each does.
2. **Skills** (`skills/` directory) — if the feature is something an AI agent
   should know how to use, ensure a corresponding SKILL.md exists and covers it.
   Update existing skills when command signatures or workflows change.

Documentation updates should be part of the same commit/PR as the feature
change, not a follow-up.
