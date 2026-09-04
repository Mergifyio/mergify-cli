---
name: releasing
description: Cut and ship a new release of the `mergify` CLI — build the draft release with its platform binaries, publish it, and verify PyPI and the Homebrew tap picked it up. Use when asked to release, cut a release, ship a new version, publish a release, tag a version, or when a release run failed and needs recovery. Triggers on "release", "new version", "cut a release", "ship it", "publish the release", "bump the version", "release failed", "PyPI publish failed".
---

# Releasing `mergify-cli`

Everything is driven by `.github/workflows/release.yml`. **There is no version
to bump in any file** — `pyproject.toml` and `Cargo.toml` keep their placeholder
versions and the workflow stamps the tag in at build time. Never open a "release
prep" PR.

Versions are calver: `YYYY.M.D.N` (no zero padding, `N` starts at 1 each UTC day).

`RELEASING.md` at the repo root is the human-facing runbook and explains *why*
the flow is shaped this way (GitHub's immutable-releases policy). Read it when
something goes wrong or the workflow itself needs changing.

## Guardrails

- **Never run `gh release create` or push a tag by hand.** A release created
  outside the workflow has no binaries, and once published it is immutable — the
  asset assertion then permanently blocks the PyPI publish for that version.
- **Never click / script "Draft a new release" in the Releases UI.** Same trap.
- **Stage 2 (Publish) is irreversible and outward-facing** — it locks the release
  and pushes to PyPI. Always get the user's explicit go-ahead before publishing,
  even if they already asked for "a release"; report the draft URL and stop.
- Stage 1 is safe and repeatable: a draft can be deleted and rebuilt.

## Stage 1 — build the draft

Pre-flight (report anything red, don't silently proceed):

```shell
gh run list --workflow=ci.yaml --branch main --limit 1              # main is green
gh release list --limit 3                                           # no leftover draft
git log --oneline $(git describe --tags --abbrev=0)..origin/main    # what ships
```

Trigger it:

```shell
gh workflow run release.yml                  # auto-picks YYYY.M.D.<next>
# or, only when a specific version is needed:
gh workflow run release.yml -f tag=2026.9.4.1 -f target_commitish=<sha>
```

Leave `tag` empty unless the user asked for a specific version; leave
`target_commitish` empty unless cherry-picking a release off an older line.

Watch it (~5–10 min):

```shell
gh run list --workflow=release.yml --limit 1
gh run watch <run-id> --exit-status
```

It builds the five-target wheel matrix, extracts the `mergify` binary from each
wheel, packages the archives + `SHA256SUMS`, dumps `cli-schema.json`, signs the
binaries with build provenance, and creates the **draft** release with notes
generated from the PRs merged since the previous tag.

Then verify — the draft must carry exactly these seven assets:

```shell
tag=$(gh release list --limit 1 --json tagName --jq '.[0].tagName')
gh release view "$tag" --json isDraft,url,assets --jq '{draft:.isDraft,url:.url,assets:[.assets[].name]}'
```

- `mergify-<tag>-x86_64-unknown-linux-gnu.tar.gz`
- `mergify-<tag>-aarch64-unknown-linux-gnu.tar.gz`
- `mergify-<tag>-x86_64-apple-darwin.tar.gz`
- `mergify-<tag>-aarch64-apple-darwin.tar.gz`
- `mergify-<tag>-x86_64-pc-windows-msvc.zip`
- `SHA256SUMS`
- `cli-schema.json`

Give the user the draft URL and the generated notes to review. **Stop here.**

## Stage 2 — publish

Only after the user explicitly says to publish:

```shell
gh release edit "$tag" --draft=false --latest
```

That fires `release: published`, which re-asserts the seven assets, rebuilds the
wheels with the same version stamp, and pushes to PyPI via Trusted Publishing
(~5–10 min). Watch the `release` run the same way as in stage 1.

Note edits must happen *before* this (`gh release edit "$tag" --notes-file
notes.md`) — drafts are mutable, published releases are not.

## Verify after publishing

```shell
gh run list --workflow=release.yml --limit 2          # publish job green
curl -s https://pypi.org/pypi/mergify-cli/json | jq -r .info.version
gh pr list --repo Mergifyio/homebrew-tap --author mergify-ci-bot
```

The Homebrew formula bump is automatic: the `homebrew-tap-sync` workflow in
`Mergifyio/mergify-ci-bot` opens a PR against `Mergifyio/homebrew-tap` within
~20 minutes of publish, updating `RELEASE` and the four per-arch checksums from
`SHA256SUMS`. It still needs a human to merge it. The docs site picks up
`cli-schema.json` off the latest release on its own — nothing to do there.

## Recovery

| Symptom | Fix |
|---|---|
| Stage 1 failed mid-run | No release exists yet, or a draft does. Delete the draft (`gh release delete <tag> --cleanup-tag`) and re-run stage 1. |
| Draft missing assets | Delete the draft and re-run stage 1 — never backfill by hand. |
| `assert-binaries-present` failed after publish | The release was created outside the workflow and is now immutable. Delete the release *and* its tag, then re-run stage 1 with the same tag. |
| PyPI publish failed (transient / outage) | Wheels are built; re-run just the failed job: `gh run rerun <run-id> --failed`. |
| Wrong version already on PyPI | PyPI versions can't be reused or overwritten. Ship the next `N` — do not try to reuse the tag. |
