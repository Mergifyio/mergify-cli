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
