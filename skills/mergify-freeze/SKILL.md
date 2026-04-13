---
name: mergify-freeze
description: Use Mergify freeze commands to create, list, update, and delete scheduled freezes that temporarily halt PR merging. ALWAYS use this skill when managing merge freezes, deployment windows, or temporarily blocking merges. Triggers on freeze, scheduled freeze, merge freeze, deployment freeze, halt merges.
---

# Mergify Scheduled Freezes

## Overview

Scheduled freezes temporarily halt merging of pull requests matching specific conditions. Use them for deployment windows, incident response, maintenance periods, or any situation where merges should be paused.

## Commands

```bash
mergify freeze list                       # List all scheduled freezes
mergify freeze list --json                # Machine-readable JSON output
mergify freeze create OPTIONS             # Create a new scheduled freeze
mergify freeze update FREEZE_ID OPTIONS   # Update an existing freeze
mergify freeze delete FREEZE_ID           # Delete a freeze
```

## Authentication

All commands require a Mergify or GitHub token:
- `--token` / `-t` (env: `MERGIFY_TOKEN` or `GITHUB_TOKEN`) -- defaults to `gh auth token`
- `--repository` / `-r` -- Repository full name (auto-detected from git remote)
- `--api-url` / `-u` (env: `MERGIFY_API_URL`) -- Mergify API URL (default: `https://api.mergify.com`)

## Creating a Freeze

```bash
# Emergency freeze (starts now, no end time)
mergify freeze create \
  --reason "Production incident - halting all merges" \
  --timezone "US/Eastern"

# Scheduled maintenance window
mergify freeze create \
  --reason "Weekend deployment freeze" \
  --timezone "Europe/Paris" \
  --start "2024-12-20T18:00:00" \
  --end "2024-12-23T08:00:00"

# Freeze with conditions (only freeze merges to main)
mergify freeze create \
  --reason "Release freeze for v2.0" \
  --timezone "UTC" \
  -c "base=main"

# Freeze with exclusions (allow hotfix PRs through)
mergify freeze create \
  --reason "Code freeze" \
  --timezone "UTC" \
  -c "base=main" \
  -e "label=hotfix"
```

**Required options:**
- `--reason` -- Human-readable reason for the freeze
- `--timezone` -- IANA timezone name (e.g., `Europe/Paris`, `US/Eastern`, `UTC`)

**Optional options:**
- `--start` -- Start time in ISO 8601 format (default: now)
- `--end` -- End time in ISO 8601 format (default: no end, emergency freeze)
- `--condition` / `-c` -- Matching condition (repeatable, e.g., `-c 'base=main'`)
- `--exclude` / `-e` -- Exclude condition (repeatable, e.g., `-e 'label=hotfix'`)

## Listing Freezes

```bash
# Table view
mergify freeze list

# JSON output for scripting
mergify freeze list --json
```

The table shows: ID, reason, start/end times with timezone, matching conditions, and active/scheduled status.

## Updating a Freeze

```bash
# Extend a freeze
mergify freeze update FREEZE_ID --end "2024-12-24T08:00:00"

# Change the reason
mergify freeze update FREEZE_ID --reason "Extended: waiting for hotfix"

# Set exclusions (replaces the full exclusion list, does not append)
mergify freeze update FREEZE_ID -e "label=emergency"
```

The `FREEZE_ID` is the UUID shown in `mergify freeze list`.

## Deleting a Freeze

```bash
# Delete a scheduled (not yet active) freeze
mergify freeze delete FREEZE_ID

# Delete an active freeze (reason required)
mergify freeze delete FREEZE_ID --reason "Incident resolved"
```

If the freeze is currently active, a `--reason` for deletion is required.

## Common Patterns

### Emergency freeze during an incident
```bash
# Stop all merges immediately
mergify freeze create \
  --reason "Incident #1234 - API outage" \
  --timezone UTC

# Once resolved, delete the freeze
mergify freeze list --json  # Get the freeze ID
mergify freeze delete FREEZE_UUID --reason "Incident #1234 resolved"
```

### Recurring deployment window
Create the freeze before each deployment window and delete it after:
```bash
mergify freeze create \
  --reason "Deploy window" \
  --timezone "US/Pacific" \
  --start "2024-12-20T14:00:00" \
  --end "2024-12-20T16:00:00"
```

### Freeze with exceptions for critical fixes
```bash
mergify freeze create \
  --reason "Sprint freeze" \
  --timezone UTC \
  -c "base=main" \
  -e "label=hotfix" \
  -e "label=security"
```
