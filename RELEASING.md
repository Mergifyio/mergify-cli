# Releasing `mergify-cli`

Two-stage flow keyed off the immutable-releases policy on
`Mergifyio/*` — once a release is published, asset metadata is
locked. So binaries are attached to the release **as a draft**
before the maintainer clicks Publish.

## Stage 1 — build the draft

1. Go to **Actions** → **release** → **Run workflow** (top right of
   the workflow page).
2. Leave **tag** empty to auto-pick `YYYY.M.D.<next>` (today's UTC
   date + one past the highest existing tag for today; `1` if you
   haven't shipped today yet). Override it only if you need a
   specific version (e.g. cherry-pick onto an older line). Leave
   **target_commitish** empty unless you're cherry-picking a
   release commit off an older branch.
3. Click **Run workflow**.

The workflow builds the wheel matrix (Linux x86_64/aarch64, macOS
x86_64/aarch64, Windows x86_64), extracts the `mergify` binary out
of each, packages `mergify-<version>-<target>.{tar.gz,zip}` +
`SHA256SUMS`, dumps the CLI schema to `cli-schema.json` (rendered
by the docs site into the command reference), and runs `gh release
create <tag> --draft --generate-notes` to create the release with
the assets attached and notes auto-generated from PRs merged since
the previous tag. Takes ~10 minutes.

## Stage 2 — review and publish

1. Go to **Releases** → the new draft.
2. Review the auto-generated notes; edit if needed (drafts are
   mutable).
3. Confirm all seven asset names are listed (the binaries are each
   prefixed with the release version):
   - `mergify-<version>-x86_64-unknown-linux-gnu.tar.gz`
   - `mergify-<version>-aarch64-unknown-linux-gnu.tar.gz`
   - `mergify-<version>-x86_64-apple-darwin.tar.gz`
   - `mergify-<version>-aarch64-apple-darwin.tar.gz`
   - `mergify-<version>-x86_64-pc-windows-msvc.zip`
   - `SHA256SUMS`
   - `cli-schema.json`
4. Click **Publish release**.

Publishing fires `release: published`, which kicks the second half
of the workflow: assert all seven assets are present, rebuild wheels
with the same version stamp, and publish to PyPI through
Trusted-Publisher. Takes ~10 minutes.

## Do not

- **Don't click "Draft a new release" in the Releases UI.** That
  path creates a draft without binaries; once you publish it the
  release is immutable and the asset assertion will fail, blocking
  PyPI. To recover you'd have to delete the release and re-run
  stage 1 with the same tag.
- **Don't run `gh release create` from your laptop.** The
  workflow does this so the binary build is reproducible and the
  PyPI Trusted-Publisher identity matches the tag.

## If stage 2 fails

The release is already published and immutable. If the assert job
fails (binaries missing) or the PyPI publish fails (transient
PyPI outage, version conflict, etc.):

- **Asset assertion failed** — someone bypassed the workflow.
  Delete the release, re-run stage 1.
- **PyPI publish failed** — re-run just the `publish` job from
  Actions. The wheels were built; PyPI is the only step left.
