//! `mergify self-update` — upgrade the running binary in place.
//!
//! Targets the `curl | sh` install channel: same release assets,
//! same `SHA256SUMS` verification, same Rust target triple
//! mapping. Users on `PyPI` / `Homebrew` should keep using their
//! package manager — we run a best-effort sanity check on the
//! binary's install path and warn (not error) when it looks
//! package-manager-owned.
//!
//! Flow:
//!
//! 1. GET `/repos/Mergifyio/mergify-cli/releases/latest` for the
//!    tag and the published asset list.
//! 2. If `tag == crate::VERSION` and `--force` wasn't passed,
//!    print "already up to date" and return.
//! 3. Pick the matching asset out of the release's published asset
//!    list by its target-triple suffix (`.zip` on Windows,
//!    `.tar.gz` elsewhere) and download it + `SHA256SUMS` via the
//!    URLs the API hands back. We *discover* the asset rather than
//!    reconstructing its filename: the release has embedded the
//!    version in the name (`mergify-<version>-<target>.<ext>`) and
//!    could rename it again, but a binary frozen at install time
//!    can't know a future scheme — matching on the stable triple
//!    suffix keeps `self-update` working across renames.
//! 4. Verify the asset SHA256 against `SHA256SUMS`. Mirrors
//!    `install.sh`'s line-shape validation so a malformed
//!    `SHA256SUMS` can't slip past.
//! 5. Shell out to `tar -xzf` for extraction (identical to
//!    `install.sh`; saves a `tar` + `flate2` crate dep).
//! 6. [`self_replace`] atomically swaps the running binary with
//!    the freshly extracted one — handles the Windows
//!    rename-over-a-running-exe case.

use std::path::Path;
use std::time::Duration;

use mergify_core::CliError;
use serde::Deserialize;
use sha2::Digest;
use sha2::Sha256;

const REPO: &str = "Mergifyio/mergify-cli";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);
const DEFAULT_API_BASE: &str = "https://api.github.com";

/// Env-var override that points the release lookup at a fixture URL.
/// Same lever `install.sh` exposes as `MERGIFY_BASE_URL` — the CI
/// smoke test sets it to a `python3 -m http.server` fixture so the
/// full download + verify + swap path runs without hitting real
/// GitHub. The fixture serves a `latest-release.json` whose
/// `assets[].browser_download_url` point back at the same server, so
/// the discovery path is identical to the real one. Unset in real
/// use.
const BASE_URL_ENV: &str = "MERGIFY_BASE_URL";

/// The release-metadata URL: the fixture stub in `MERGIFY_BASE_URL`
/// mode, otherwise GitHub's `releases/latest` API. Asset URLs are
/// read from the response, never reconstructed, so we only need to
/// know where the metadata lives.
fn latest_release_url() -> String {
    if let Ok(base) = std::env::var(BASE_URL_ENV) {
        format!("{base}/latest-release.json")
    } else {
        format!("{DEFAULT_API_BASE}/repos/{REPO}/releases/latest")
    }
}

/// Inputs to [`run`]. Construction is owned by the CLI binding in
/// `main.rs` so this module stays UI-agnostic.
#[derive(Debug)]
pub struct Options {
    /// Re-install even when the running binary already matches the
    /// latest release. Useful for repairing a corrupted install.
    pub force: bool,
    /// Resolve and print the latest release tag, then exit without
    /// touching the binary.
    pub check_only: bool,
}

#[derive(Deserialize)]
struct LatestRelease {
    tag_name: String,
    /// The release's published assets. GitHub returns more fields
    /// per asset; we only deserialize what we download by.
    #[serde(default)]
    assets: Vec<Asset>,
}

#[derive(Debug, Deserialize)]
struct Asset {
    name: String,
    browser_download_url: String,
}

/// Pick the single release asset whose name ends in
/// `-<target>.<ext>`. Matching the *suffix* — not the full
/// reconstructed name — is what makes `self-update` survive an
/// asset-naming change: the prefix (`mergify-`, an embedded version,
/// or whatever a future release uses) is ignored, only the stable
/// target-triple tail has to line up. Fails closed if zero or more
/// than one asset matches.
fn select_asset<'a>(assets: &'a [Asset], target: &str, ext: &str) -> Result<&'a Asset, CliError> {
    let suffix = format!("-{target}.{ext}");
    let mut matching = assets.iter().filter(|a| a.name.ends_with(&suffix));
    let found = matching.next().ok_or_else(|| {
        CliError::Generic(format!(
            "no release asset matching *{suffix} — the latest release ships no binary for this platform"
        ))
    })?;
    if matching.next().is_some() {
        return Err(CliError::Generic(format!(
            "multiple release assets match *{suffix}; refusing to guess"
        )));
    }
    Ok(found)
}

pub async fn run(opts: &Options) -> Result<(), CliError> {
    let current = crate::VERSION;
    let target = current_target()?;
    let client = http_client()?;
    let release = fetch_latest_release(&client, &latest_release_url()).await?;
    let latest = release.tag_name.as_str();

    if opts.check_only {
        println!("Current: {current}");
        println!("Latest:  {latest}");
        return Ok(());
    }
    if current == latest && !opts.force {
        println!("mergify is up to date ({current})");
        return Ok(());
    }
    println!("Updating mergify: {current} -> {latest}");

    // Windows ships `.zip`, every other target `.tar.gz`. The
    // release workflow packages assets the same way, so the
    // extension is determined by `cfg!(windows)` here, not by
    // target string-sniffing.
    let ext = if cfg!(windows) { "zip" } else { "tar.gz" };
    let asset = select_asset(&release.assets, target, ext)?;
    let sums_asset = release
        .assets
        .iter()
        .find(|a| a.name == "SHA256SUMS")
        .ok_or_else(|| CliError::Generic("latest release has no SHA256SUMS asset".to_string()))?;
    let archive = download(&client, &asset.browser_download_url).await?;
    let sums = download_text(&client, &sums_asset.browser_download_url).await?;
    // Verify against the asset's *actual* published name, so the
    // `SHA256SUMS` lookup matches whatever scheme the release used.
    verify_checksum(&archive, &asset.name, &sums)?;
    let asset_name = asset.name.clone();

    // Stage the extracted binary in a sibling temp dir next to the
    // current binary. Same filesystem keeps the final `rename` in
    // [`self_replace`] atomic — on Linux/macOS a cross-fs rename
    // would silently copy + delete and lose the "no half-written
    // binary on disk" guarantee.
    let current_exe = std::env::current_exe()
        .map_err(|e| CliError::Generic(format!("locate current binary: {e}")))?;
    let install_dir = current_exe
        .parent()
        .ok_or_else(|| CliError::Generic("current binary has no parent dir".to_string()))?;
    warn_if_package_manager_owned(install_dir);

    let workdir = tempfile::tempdir_in(install_dir)
        .map_err(|e| CliError::Generic(format!("create temp dir near binary: {e}")))?;
    // `asset_name` comes from release metadata. Even though the
    // archive's checksum is verified, the filename itself is
    // untrusted input — constrain it to a bare leaf component so a
    // crafted name with path separators (`../`, an absolute path)
    // can't make us write the archive outside `workdir`.
    let leaf = Path::new(&asset_name).file_name().ok_or_else(|| {
        CliError::Generic(format!("asset name is not a plain filename: {asset_name}"))
    })?;
    let archive_path = workdir.path().join(leaf);
    std::fs::write(&archive_path, &archive)
        .map_err(|e| CliError::Generic(format!("write archive: {e}")))?;

    extract(&archive_path, workdir.path())?;
    let bin_name = if cfg!(windows) {
        "mergify.exe"
    } else {
        "mergify"
    };
    let new_bin = workdir.path().join(bin_name);
    if !new_bin.exists() {
        return Err(CliError::Generic(format!(
            "archive {asset_name} did not contain the expected binary {bin_name}"
        )));
    }

    // `self_replace::self_replace` does the atomic swap. On Unix
    // this is a `rename(new, current)` over the running binary
    // (kernel keeps the in-use inode alive). On Windows it renames
    // the current binary aside, then renames the new one into
    // place — there's no truly atomic `rename`-over-a-running-exe
    // on Windows, but the orphaned `.old` file is cleaned up on
    // the next process exit.
    self_replace::self_replace(&new_bin)
        .map_err(|e| CliError::Generic(format!("replace running binary: {e}")))?;

    println!("Installed mergify {latest} to {}", current_exe.display());
    Ok(())
}

fn http_client() -> Result<reqwest::Client, CliError> {
    reqwest::Client::builder()
        .timeout(REQUEST_TIMEOUT)
        .user_agent(concat!("mergify-cli/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|e| CliError::Generic(format!("build HTTP client: {e}")))
}

async fn fetch_latest_release(
    client: &reqwest::Client,
    url: &str,
) -> Result<LatestRelease, CliError> {
    client
        .get(url)
        .send()
        .await
        .map_err(|e| CliError::Generic(format!("GET {url}: {e}")))?
        .error_for_status()
        .map_err(|e| CliError::Generic(format!("GitHub API: {e}")))?
        .json::<LatestRelease>()
        .await
        .map_err(|e| CliError::Generic(format!("parse latest-release: {e}")))
}

async fn download(client: &reqwest::Client, url: &str) -> Result<Vec<u8>, CliError> {
    Ok(client
        .get(url)
        .send()
        .await
        .map_err(|e| CliError::Generic(format!("GET {url}: {e}")))?
        .error_for_status()
        .map_err(|e| CliError::Generic(format!("GET {url}: {e}")))?
        .bytes()
        .await
        .map_err(|e| CliError::Generic(format!("read body {url}: {e}")))?
        .to_vec())
}

async fn download_text(client: &reqwest::Client, url: &str) -> Result<String, CliError> {
    let bytes = download(client, url).await?;
    String::from_utf8(bytes)
        .map_err(|e| CliError::Generic(format!("non-UTF8 body from {url}: {e}")))
}

/// Render digest bytes as a lowercase hex string. `sha2` 0.11 returns
/// a `hybrid_array::Array` from `finalize()` that no longer implements
/// `LowerHex`, so we format the bytes ourselves.
fn hex_encode(bytes: &[u8]) -> String {
    use std::fmt::Write;
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(s, "{b:02x}");
    }
    s
}

/// Verify the downloaded archive's SHA256 against the entry for
/// `asset_name` in `SHA256SUMS`. Mirrors `install.sh` exactly: the
/// line must split as `<hash> <name>` on whitespace where the
/// second field equals `asset_name` *literally* (not `ends_with`,
/// so `mergify-2099.1.1.1-fooX-target.tar.gz` can't accidentally pass the
/// check for `target.tar.gz`), and the hash field must be 64 hex
/// chars. Both layers fail closed.
fn verify_checksum(archive: &[u8], asset_name: &str, sums: &str) -> Result<(), CliError> {
    let mut expected: Option<&str> = None;
    for line in sums.lines() {
        let mut fields = line.split_whitespace();
        let Some(hash) = fields.next() else { continue };
        let Some(name) = fields.next() else { continue };
        // Canonical `sha256sum` output is exactly two fields; bail
        // on anything else even if `name == asset_name`, so a
        // doctored entry with smuggled extras can't sneak past.
        if fields.next().is_some() {
            continue;
        }
        if name == asset_name {
            expected = Some(hash);
            break;
        }
    }
    let expected = expected.ok_or_else(|| {
        CliError::Generic(format!("no checksum entry for {asset_name} in SHA256SUMS"))
    })?;
    if expected.len() != 64 || !expected.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(CliError::Generic(format!(
            "malformed checksum entry for {asset_name}"
        )));
    }
    let mut hasher = Sha256::new();
    hasher.update(archive);
    let actual = hex_encode(&hasher.finalize());
    if actual.eq_ignore_ascii_case(expected) {
        Ok(())
    } else {
        Err(CliError::Generic(format!(
            "checksum mismatch for {asset_name}: expected {expected}, got {actual}"
        )))
    }
}

/// Extract `archive` (a `.tar.gz` or `.zip`) into `dest` by
/// shelling out to the system `tar` / PowerShell `Expand-Archive`.
/// `install.sh` shells out to `tar` too — keeping the same
/// extractor for both code paths keeps "did the archive parse OK"
/// surface area tiny.
fn extract(archive: &Path, dest: &Path) -> Result<(), CliError> {
    let archive_str = archive.to_string_lossy();
    let dest_str = dest.to_string_lossy();
    let status = if archive_str.ends_with(".zip") {
        // PowerShell single-quoted strings escape an embedded `'`
        // by doubling it. Without this, a Windows username like
        // `C:\Users\O'Connor\...` would terminate the literal
        // early and PowerShell would refuse the command.
        let archive_quoted = archive_str.replace('\'', "''");
        let dest_quoted = dest_str.replace('\'', "''");
        std::process::Command::new("powershell")
            .args([
                "-NoProfile",
                "-Command",
                &format!(
                    "Expand-Archive -LiteralPath '{archive_quoted}' \
                     -DestinationPath '{dest_quoted}' -Force"
                ),
            ])
            .status()
    } else {
        std::process::Command::new("tar")
            .args(["-xzf", &archive_str, "-C", &dest_str])
            .status()
    };
    let status = status.map_err(|e| CliError::Generic(format!("spawn extractor: {e}")))?;
    if !status.success() {
        return Err(CliError::Generic(format!(
            "extractor exited with status {status}"
        )));
    }
    Ok(())
}

/// Map the running platform to the Rust target triple the release
/// workflow tags its assets with. Mirrors `install.sh`'s
/// `detect_target` — kept here as a sanity net (the release wheel
/// matrix wouldn't have shipped a binary for an unsupported
/// target, so the running binary can only exist on a known one),
/// but we still match explicitly so a future cross-build for a
/// new triple gets caught before the GitHub URL 404s.
fn current_target() -> Result<&'static str, CliError> {
    let target = match (std::env::consts::OS, std::env::consts::ARCH) {
        ("linux", "x86_64") => "x86_64-unknown-linux-gnu",
        ("linux", "aarch64") => "aarch64-unknown-linux-gnu",
        ("macos", "x86_64") => "x86_64-apple-darwin",
        ("macos", "aarch64") => "aarch64-apple-darwin",
        ("windows", "x86_64") => "x86_64-pc-windows-msvc",
        (os, arch) => {
            return Err(CliError::Generic(format!(
                "no release binary published for {os}/{arch} — install from source"
            )));
        }
    };
    Ok(target)
}

/// Print a notice (not an error) if the install path looks
/// package-manager-owned. Updating in place still works, but the
/// package manager will overwrite our binary on its next upgrade.
fn warn_if_package_manager_owned(install_dir: &Path) {
    // Conservative list — match the path prefixes the common
    // installers use, not arbitrary substrings, to avoid false
    // positives. Skipping `~/.local/bin` (where the curl|sh
    // installer lives) and `/usr/local/bin` (often used manually).
    let path = install_dir.to_string_lossy();
    let owned_by: Option<&str> = if path.contains("/Cellar/") || path.starts_with("/opt/homebrew/")
    {
        Some("Homebrew")
    } else if path.contains("/.local/share/uv/tools/") || path.contains("/.local/pipx/") {
        Some("uv / pipx")
    } else if path.contains("/site-packages/") {
        Some("pip")
    } else {
        None
    };
    if let Some(mgr) = owned_by {
        eprintln!(
            "warning: mergify looks like it was installed by {mgr}. \
             `self-update` will overwrite the binary, but {mgr} will \
             restore its own version on the next upgrade. Use \
             {mgr}'s upgrade command instead for a durable update."
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_archive() -> Vec<u8> {
        b"pretend-this-is-a-tar.gz".to_vec()
    }

    fn asset(name: &str) -> Asset {
        Asset {
            name: name.to_string(),
            browser_download_url: format!("https://example.test/{name}"),
        }
    }

    #[test]
    fn select_asset_matches_versioned_name() {
        let assets = [
            asset("mergify-2099.1.1.1-x86_64-unknown-linux-gnu.tar.gz"),
            asset("mergify-2099.1.1.1-aarch64-apple-darwin.tar.gz"),
            asset("SHA256SUMS"),
        ];
        let found = select_asset(&assets, "aarch64-apple-darwin", "tar.gz").unwrap();
        assert_eq!(found.name, "mergify-2099.1.1.1-aarch64-apple-darwin.tar.gz");
    }

    #[test]
    fn select_asset_matches_legacy_unversioned_name() {
        // The whole point of suffix-matching: a binary built after a
        // future rename must still resolve assets from an
        // old-scheme release (here the pre-#1603 name with no
        // embedded version). The prefix is irrelevant.
        let assets = [
            asset("mergify-aarch64-apple-darwin.tar.gz"),
            asset("SHA256SUMS"),
        ];
        let found = select_asset(&assets, "aarch64-apple-darwin", "tar.gz").unwrap();
        assert_eq!(found.name, "mergify-aarch64-apple-darwin.tar.gz");
    }

    #[test]
    fn select_asset_errors_when_platform_absent() {
        let assets = [
            asset("mergify-2099.1.1.1-x86_64-pc-windows-msvc.zip"),
            asset("SHA256SUMS"),
        ];
        let err = select_asset(&assets, "aarch64-apple-darwin", "tar.gz").unwrap_err();
        assert!(err.to_string().contains("no release asset"), "got: {err}");
    }

    #[test]
    fn select_asset_rejects_ambiguous_matches() {
        // Two assets with the same target suffix — refuse rather
        // than silently grabbing the first.
        let assets = [
            asset("mergify-2099.1.1.1-x86_64-apple-darwin.tar.gz"),
            asset("mergify-2099.1.1.2-x86_64-apple-darwin.tar.gz"),
        ];
        let err = select_asset(&assets, "x86_64-apple-darwin", "tar.gz").unwrap_err();
        assert!(
            err.to_string().contains("multiple release assets"),
            "got: {err}"
        );
    }

    #[test]
    fn verify_checksum_accepts_a_matching_entry() {
        let archive = fixture_archive();
        let mut h = Sha256::new();
        h.update(&archive);
        let hash = hex_encode(&h.finalize());
        let sums = format!("{hash}  mergify-2099.1.1.1-x86_64-unknown-linux-gnu.tar.gz\n");
        verify_checksum(
            &archive,
            "mergify-2099.1.1.1-x86_64-unknown-linux-gnu.tar.gz",
            &sums,
        )
        .unwrap();
    }

    #[test]
    fn verify_checksum_rejects_mismatch() {
        let archive = fixture_archive();
        let wrong = "0".repeat(64);
        let sums = format!("{wrong}  mergify-2099.1.1.1-x86_64-unknown-linux-gnu.tar.gz\n");
        let err = verify_checksum(
            &archive,
            "mergify-2099.1.1.1-x86_64-unknown-linux-gnu.tar.gz",
            &sums,
        )
        .unwrap_err();
        assert!(err.to_string().contains("checksum mismatch"));
    }

    #[test]
    fn verify_checksum_rejects_missing_entry() {
        let sums = "deadbeef  mergify-2099.1.1.1-aarch64-apple-darwin.tar.gz\n";
        let err = verify_checksum(
            &[],
            "mergify-2099.1.1.1-x86_64-unknown-linux-gnu.tar.gz",
            sums,
        )
        .unwrap_err();
        assert!(err.to_string().contains("no checksum entry"));
    }

    #[test]
    fn verify_checksum_does_not_accept_suffix_match() {
        // Regression for the pre-fix `ends_with` lookup: a sibling
        // asset whose name *ends in* `asset_name` (here
        // `mergify-2099.1.1.1-x86_64-pc-windows-msvc.zip` ending in
        // `.zip`) could be matched and pass even when the requested
        // asset wasn't in the file. The literal second-field match
        // must reject this.
        let archive = fixture_archive();
        let mut h = Sha256::new();
        h.update(&archive);
        let hash = hex_encode(&h.finalize());
        let sums = format!("{hash}  mergify-2099.1.1.1-x86_64-pc-windows-msvc.zip\n");
        let err = verify_checksum(&archive, "msvc.zip", &sums).unwrap_err();
        assert!(
            err.to_string().contains("no checksum entry"),
            "expected 'no checksum entry', got: {err}",
        );
    }

    #[test]
    fn verify_checksum_rejects_extra_fields_on_the_entry_line() {
        // Defence-in-depth against a doctored SHA256SUMS smuggling
        // extra fields after the asset name (e.g. an injected
        // `; rm -rf /` for a downstream shell consumer). Canonical
        // `sha256sum` output is exactly two fields, anything else
        // is treated as a missing entry.
        let archive = fixture_archive();
        let mut h = Sha256::new();
        h.update(&archive);
        let hash = hex_encode(&h.finalize());
        let sums = format!("{hash}  mergify-2099.1.1.1-x86_64-unknown-linux-gnu.tar.gz extra\n");
        let err = verify_checksum(
            &archive,
            "mergify-2099.1.1.1-x86_64-unknown-linux-gnu.tar.gz",
            &sums,
        )
        .unwrap_err();
        assert!(
            err.to_string().contains("no checksum entry"),
            "expected 'no checksum entry', got: {err}",
        );
    }

    #[test]
    fn verify_checksum_rejects_malformed_hash_field() {
        // Right asset name, wrong-shape hash. install.sh validates
        // the same way so a corrupted SHA256SUMS can't slip past
        // sha256sum's warn-but-pass behaviour; mirror it here.
        let sums = "bogus  mergify-2099.1.1.1-x86_64-unknown-linux-gnu.tar.gz\n";
        let err = verify_checksum(
            &[],
            "mergify-2099.1.1.1-x86_64-unknown-linux-gnu.tar.gz",
            sums,
        )
        .unwrap_err();
        assert!(err.to_string().contains("malformed checksum entry"));
    }

    #[test]
    fn current_target_picks_one_of_the_known_triples() {
        // We don't ship for anything else, so the call must
        // succeed in the test harness — and it must return one of
        // the five tagged triples we publish assets for.
        let target = current_target().unwrap();
        assert!(
            [
                "x86_64-unknown-linux-gnu",
                "aarch64-unknown-linux-gnu",
                "x86_64-apple-darwin",
                "aarch64-apple-darwin",
                "x86_64-pc-windows-msvc",
            ]
            .contains(&target),
            "unexpected target: {target}",
        );
    }
}
