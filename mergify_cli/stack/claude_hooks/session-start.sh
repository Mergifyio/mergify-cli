#!/usr/bin/env bash
# Capture Claude session ID and export it for git hooks

INPUT=$(cat)

# Check if jq is available
if ! command -v jq >/dev/null 2>&1; then
    exit 0
fi

# Extract session ID, handling invalid JSON gracefully
if ! CLAUDE_SESSION_ID=$(echo "$INPUT" | jq -er '.session_id // empty' 2>/dev/null); then
    CLAUDE_SESSION_ID=""
fi

if [ -n "$CLAUDE_ENV_FILE" ] && [ -n "$CLAUDE_SESSION_ID" ]; then
    printf 'export CLAUDE_SESSION_ID=%q\n' "$CLAUDE_SESSION_ID" >> "$CLAUDE_ENV_FILE"
fi

exit 0
