//! Where the CLI keeps the Mergify credential `mergify auth login`
//! mints.
//!
//! Two backends, in this order:
//!
//! 1. **The OS keychain** — macOS Keychain, the Windows credential
//!    manager, or a freedesktop Secret Service over D-Bus.
//! 2. **A `0600` JSON file** under the user's configuration
//!    directory, used whenever the keychain is absent or refuses.
//!    A container, a CI runner, or an SSH session on a headless box
//!    has no D-Bus session at all, and `auth login` has to work
//!    there — so a keychain failure is never fatal, only a fallback.
//!
//! Entries are keyed by **API URL**, not by "the Mergify token":
//! one machine can legitimately hold a credential for the hosted
//! service and one for an on-premise install at the same time, and
//! each has to survive the other's `logout`.
//!
//! Nothing here logs a secret. The `Debug` impls below exist so
//! callers can trace *where* a credential came from without the
//! token itself reaching a log line.

use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::Path;
use std::path::PathBuf;

use chrono::DateTime;
use chrono::Utc;
use serde::Deserialize;
use serde::Serialize;
use url::Url;

use crate::error::CliError;

/// Keychain service name every entry is filed under. The account
/// within it is the API URL.
const KEYRING_SERVICE: &str = "mergify-cli";

/// Directory under the platform config directory, and the file in
/// it. Named for its content so a future `config.json` next to it
/// stays obviously non-secret.
const CONFIG_SUBDIR: &str = "mergify";
const CREDENTIALS_FILE: &str = "credentials.json";

/// A stored Mergify credential.
///
/// `expires_at` is what the server said when it minted the token,
/// and it is advisory: a token can be revoked from the dashboard
/// long before it expires, so a client that trusts this field
/// instead of the API's answer will be wrong. It is here so
/// `auth status` can say when the credential runs out without a
/// round trip.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct Credential {
    pub token: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<DateTime<Utc>>,
}

/// Which backend a credential was read from or written to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Location {
    Keychain,
    File(PathBuf),
}

impl std::fmt::Display for Location {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Keychain => f.write_str("the system keychain"),
            Self::File(path) => write!(f, "{}", path.display()),
        }
    }
}

/// A credential together with the backend it came out of, so
/// `auth status` can tell the user where their secret actually
/// lives.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoredCredential {
    pub credential: Credential,
    pub location: Location,
}

/// The two-backend credential store.
pub struct CredentialStore {
    /// `false` skips the keychain entirely. Tests set it so a unit
    /// test never reaches — or worse, writes to — the developer's
    /// real keychain.
    use_keychain: bool,
    /// `None` when the platform offers no configuration directory.
    /// Only the *fallback* needs a path, so a machine without one
    /// still uses its keychain — and that is exactly the kind of
    /// machine that has one: a Windows service account, a systemd
    /// unit with `ProtectHome=`, anything running without a home.
    file: Option<PathBuf>,
}

impl CredentialStore {
    /// The store the binary uses: OS keychain first, then a `0600`
    /// file under the platform configuration directory.
    ///
    /// Infallible on purpose. A machine with no configuration
    /// directory has no *fallback*, which is not the same as having
    /// no store: failing here would refuse to read a keychain entry
    /// that is sitting right there.
    #[must_use]
    pub fn discover() -> Self {
        Self {
            use_keychain: true,
            file: credentials_file(),
        }
    }

    /// A keychain-free store whose file lives directly at `path`.
    /// Tests use it; so does any caller that has already decided
    /// the keychain is not an option.
    #[must_use]
    pub fn file_at(path: PathBuf) -> Self {
        Self {
            use_keychain: false,
            file: Some(path),
        }
    }

    /// Read the credential stored for `api_url`, if there is one.
    ///
    /// Checks the keychain first and the file second, so a machine
    /// that once fell back to the file and later gained a working
    /// keychain reads the newer entry.
    pub fn get(&self, api_url: &Url) -> Result<Option<StoredCredential>, CliError> {
        let key = key_for(api_url);
        if self.use_keychain
            && let Some(raw) = keychain_get(&key)
        {
            let credential = parse_secret(&raw)?;
            return Ok(Some(StoredCredential {
                credential,
                location: Location::Keychain,
            }));
        }
        let Some(file) = &self.file else {
            return Ok(None);
        };
        let Some(credential) = read_file(file)?.remove(&key) else {
            return Ok(None);
        };
        Ok(Some(StoredCredential {
            credential,
            location: Location::File(file.clone()),
        }))
    }

    /// Store `credential` for `api_url` and report where it landed.
    ///
    /// The backend that did *not* take it is cleared, so a machine
    /// that logged in once without a keychain and once with it does
    /// not leave the older secret readable on disk.
    ///
    /// # Errors
    ///
    /// [`CliError::Configuration`] when neither backend is
    /// available: no keychain answered and there is nowhere to put a
    /// file.
    pub fn set(&self, api_url: &Url, credential: &Credential) -> Result<Location, CliError> {
        let key = key_for(api_url);
        let secret = serde_json::to_string(credential)
            .map_err(|e| CliError::wrap("serialize the credential", e))?;
        if self.use_keychain && keychain_set(&key, &secret) {
            // Best effort: the credential is safely in the keychain,
            // which `get` reads first, so a stale file copy is
            // shadowed rather than dangerous. Failing here — a
            // corrupt `credentials.json` makes this error — would
            // report a failed login that in fact stored a token, and
            // every retry would mint another one nobody revokes.
            if let Err(e) = self.remove_from_file(&key) {
                tracing::debug!(error = %e, "could not clear the fallback credential file");
            }
            return Ok(Location::Keychain);
        }
        // The keychain refused the write. If it is nevertheless
        // holding an older entry for this URL, writing the fallback
        // would store a credential that can never be read: `get`
        // checks the keychain first and would keep answering with
        // the stale one. Read back rather than trust the delete,
        // because "no keychain on this machine" and "keychain
        // refused" arrive as the same platform error.
        if self.use_keychain {
            keychain_delete(&key);
            if keychain_get(&key).is_some() {
                return Err(CliError::Configuration(format!(
                    "an older credential for {api_url} is in the system keychain and could \
                     not be replaced or removed. Unlock your keychain and try again.",
                )));
            }
        }
        let Some(file) = &self.file else {
            return Err(CliError::Configuration(
                "no system keychain answered, and there is no configuration directory to \
                 store the credential in (is HOME set?)"
                    .to_string(),
            ));
        };
        let mut entries = read_file(file)?;
        entries.insert(key.clone(), credential.clone());
        write_file(file, &entries)?;
        Ok(Location::File(file.clone()))
    }

    /// Forget the credential for `api_url` in both backends.
    /// Returns whether either of them held one.
    ///
    /// # Errors
    ///
    /// [`CliError::Configuration`] when the keychain is still
    /// holding the credential afterwards. `logout` promises a
    /// machine that no longer carries it, and a keychain locked
    /// against a delete looks exactly like one that had nothing —
    /// so the entry is read back rather than the delete believed.
    pub fn delete(&self, api_url: &Url) -> Result<bool, CliError> {
        let key = key_for(api_url);
        let from_file = self.remove_from_file(&key)?;
        let mut from_keychain = false;
        if self.use_keychain {
            from_keychain = keychain_delete(&key);
            if keychain_get(&key).is_some() {
                return Err(CliError::Configuration(format!(
                    "the credential for {api_url} is still in the system keychain: it could \
                     not be removed. Unlock your keychain and run `mergify auth logout` again.",
                )));
            }
        }
        Ok(from_keychain || from_file)
    }

    /// Path of the file backend, or `None` when this machine has no
    /// configuration directory to put one in.
    #[must_use]
    pub fn file_path(&self) -> Option<&Path> {
        self.file.as_deref()
    }

    fn remove_from_file(&self, key: &str) -> Result<bool, CliError> {
        let Some(file) = &self.file else {
            return Ok(false);
        };
        let mut entries = read_file(file)?;
        if entries.remove(key).is_none() {
            return Ok(false);
        }
        write_file(file, &entries)?;
        Ok(true)
    }
}

fn read_file(file: &Path) -> Result<BTreeMap<String, Credential>, CliError> {
    let raw = match fs::read(file) {
        Ok(raw) => raw,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(BTreeMap::new()),
        Err(e) => {
            return Err(CliError::wrap(format!("read {}", file.display()), e));
        }
    };
    serde_json::from_slice(&raw).map_err(|e| {
        // Not "you are logged out": a corrupt store would send
        // the user round `auth login` forever without ever
        // saying which file to look at.
        CliError::Configuration(format!(
            "{} is not valid credential JSON ({e}). Delete it and run `mergify auth login` again.",
            file.display(),
        ))
    })
}

fn write_file(file: &Path, entries: &BTreeMap<String, Credential>) -> Result<(), CliError> {
    if let Some(parent) = file.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| CliError::wrap(format!("create {}", parent.display()), e))?;
        restrict_to_owner(parent, 0o700)?;
    }
    // An empty store is a deleted file rather than `{}`: leaving
    // an empty JSON object behind reads, to anyone auditing the
    // machine, like a credential that failed to load.
    if entries.is_empty() {
        match fs::remove_file(file) {
            Ok(()) => return Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => {
                return Err(CliError::wrap(format!("remove {}", file.display()), e));
            }
        }
    }
    let rendered = serde_json::to_vec_pretty(entries)
        .map_err(|e| CliError::wrap("serialize the credential store", e))?;
    // Written to a sibling temp file and renamed, so a crash
    // mid-write cannot truncate a store that holds a second
    // deployment's credential. `NamedTempFile` creates at 0600
    // on Unix; the explicit chmod covers the umask-independent
    // guarantee and documents the intent.
    let mut tmp = tempfile::NamedTempFile::new_in(
        file.parent()
            .ok_or_else(|| CliError::Configuration("credential path has no parent".into()))?,
    )
    .map_err(|e| CliError::wrap("create a temporary credential file", e))?;
    restrict_to_owner(tmp.path(), 0o600)?;
    tmp.write_all(&rendered)
        .map_err(|e| CliError::wrap("write the credential store", e))?;
    tmp.flush()
        .map_err(|e| CliError::wrap("flush the credential store", e))?;
    tmp.persist(file)
        .map_err(|e| CliError::wrap(format!("write {}", file.display()), e.error))?;
    Ok(())
}

/// Restrict `path` to its owner. A no-op off Unix, where the file
/// inherits the ACL of the user's profile directory and there is no
/// mode to set.
#[cfg(unix)]
fn restrict_to_owner(path: &Path, mode: u32) -> Result<(), CliError> {
    use std::os::unix::fs::PermissionsExt;

    fs::set_permissions(path, fs::Permissions::from_mode(mode))
        .map_err(|e| CliError::wrap(format!("restrict permissions on {}", path.display()), e))
}

#[cfg(not(unix))]
fn restrict_to_owner(_path: &Path, _mode: u32) -> Result<(), CliError> {
    Ok(())
}

/// The account name an API URL is filed under, in both backends.
///
/// `Url::as_str` normalizes the scheme, the case, and the default
/// port, but not a trailing slash on the path — and a trailing slash
/// is not a different deployment. Every request the client makes
/// joins an absolute path (`/v1/user`), which replaces the base
/// path outright, so `https://host/api` and `https://host/api/`
/// reach byte-identical endpoints. Two entries for them would tell a
/// user who logged in with one spelling that they are not logged in
/// with the other.
fn key_for(api_url: &Url) -> String {
    let raw = api_url.as_str();
    raw.strip_suffix('/').unwrap_or(raw).to_string()
}

fn parse_secret(raw: &str) -> Result<Credential, CliError> {
    serde_json::from_str(raw).map_err(|e| {
        CliError::Configuration(format!(
            "the credential in the system keychain is not valid JSON ({e}). \
             Run `mergify auth logout` then `mergify auth login` again.",
        ))
    })
}

/// Read one keychain entry. Every failure — no entry, no keychain,
/// a locked one, a user who clicked Deny — collapses to `None`: the
/// caller's next move is the file backend either way, and the
/// distinction is only worth a debug line.
fn keychain_get(key: &str) -> Option<String> {
    match keyring::Entry::new(KEYRING_SERVICE, key).and_then(|e| e.get_password()) {
        Ok(secret) => Some(secret),
        Err(keyring::Error::NoEntry) => None,
        Err(e) => {
            tracing::debug!(error = %e, "no credential read from the system keychain");
            None
        }
    }
}

/// Write one keychain entry, reporting whether it took. `false`
/// means the caller falls back to the file.
fn keychain_set(key: &str, secret: &str) -> bool {
    match keyring::Entry::new(KEYRING_SERVICE, key).and_then(|e| e.set_password(secret)) {
        Ok(()) => true,
        Err(e) => {
            tracing::debug!(error = %e, "could not store the credential in the system keychain");
            false
        }
    }
}

/// Delete one keychain entry, reporting whether one was there.
fn keychain_delete(key: &str) -> bool {
    match keyring::Entry::new(KEYRING_SERVICE, key).and_then(|e| e.delete_credential()) {
        Ok(()) => true,
        Err(keyring::Error::NoEntry) => false,
        Err(e) => {
            tracing::debug!(error = %e, "could not delete the credential from the system keychain");
            false
        }
    }
}

/// Where the file backend lives, or `None` on a machine that gives
/// no configuration directory — which on every supported platform
/// means the environment does not name a home.
fn credentials_file() -> Option<PathBuf> {
    let Some(dir) = dirs::config_dir() else {
        tracing::debug!("no configuration directory; the credential store is keychain-only");
        return None;
    };
    Some(dir.join(CONFIG_SUBDIR).join(CREDENTIALS_FILE))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store(dir: &tempfile::TempDir) -> CredentialStore {
        CredentialStore::file_at(dir.path().join("mergify").join("credentials.json"))
    }

    fn url(raw: &str) -> Url {
        Url::parse(raw).unwrap()
    }

    fn credential(token: &str) -> Credential {
        Credential {
            token: token.to_string(),
            expires_at: None,
        }
    }

    #[test]
    fn get_returns_none_when_nothing_is_stored() {
        let dir = tempfile::tempdir().unwrap();
        let store = store(&dir);
        assert_eq!(store.get(&url("https://api.mergify.com")).unwrap(), None);
    }

    #[test]
    fn set_then_get_round_trips_through_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let store = store(&dir);
        let api = url("https://api.mergify.com");
        let location = store.set(&api, &credential("mut_secret")).unwrap();
        assert_eq!(
            location,
            Location::File(store.file_path().unwrap().to_path_buf())
        );

        let found = store.get(&api).unwrap().unwrap();
        assert_eq!(found.credential, credential("mut_secret"));
        assert_eq!(
            found.location,
            Location::File(store.file_path().unwrap().into())
        );
    }

    #[test]
    fn expiry_survives_the_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let store = store(&dir);
        let api = url("https://api.mergify.com");
        let expires_at = DateTime::parse_from_rfc3339("2027-01-02T03:04:05Z")
            .unwrap()
            .with_timezone(&Utc);
        let credential = Credential {
            token: "mut_secret".to_string(),
            expires_at: Some(expires_at),
        };
        store.set(&api, &credential).unwrap();
        assert_eq!(store.get(&api).unwrap().unwrap().credential, credential);
    }

    // The whole point of keying by API URL: a laptop that talks to
    // SaaS and to an on-premise install holds both, and neither
    // login overwrites the other.
    #[test]
    fn credentials_are_keyed_by_api_url() {
        let dir = tempfile::tempdir().unwrap();
        let store = store(&dir);
        let saas = url("https://api.mergify.com");
        let onprem = url("https://mergify.internal.example/api");
        store.set(&saas, &credential("mut_saas")).unwrap();
        store.set(&onprem, &credential("mut_onprem")).unwrap();

        assert_eq!(
            store.get(&saas).unwrap().unwrap().credential.token,
            "mut_saas",
        );
        assert_eq!(
            store.get(&onprem).unwrap().unwrap().credential.token,
            "mut_onprem",
        );

        assert!(store.delete(&saas).unwrap());
        assert_eq!(store.get(&saas).unwrap(), None);
        assert_eq!(
            store.get(&onprem).unwrap().unwrap().credential.token,
            "mut_onprem",
            "deleting one deployment's credential must not touch another's",
        );
    }

    // `https://api.mergify.com` and `https://api.mergify.com/` are
    // the same deployment, and `--api-url` accepts both spellings.
    // `Url` normalizes this pair on its own, which is why the
    // on-premise pair below is the one that actually pins `key_for`.
    #[test]
    fn url_spellings_resolve_to_one_entry() {
        let dir = tempfile::tempdir().unwrap();
        let store = store(&dir);
        store
            .set(&url("https://api.mergify.com"), &credential("mut_one"))
            .unwrap();
        assert_eq!(
            store
                .get(&url("https://api.mergify.com/"))
                .unwrap()
                .unwrap()
                .credential
                .token,
            "mut_one",
        );
    }

    // The pair `Url` does *not* normalize, and the one an
    // on-premise deployment is actually spelled with. Both bases
    // reach the same endpoint, because every request joins an
    // absolute path over them.
    #[test]
    fn a_trailing_slash_on_the_path_is_the_same_deployment() {
        let dir = tempfile::tempdir().unwrap();
        let store = store(&dir);
        store
            .set(
                &url("https://mergify.internal.example/api"),
                &credential("mut_onprem"),
            )
            .unwrap();
        assert_eq!(
            store
                .get(&url("https://mergify.internal.example/api/"))
                .unwrap()
                .unwrap()
                .credential
                .token,
            "mut_onprem",
        );
    }

    #[test]
    fn delete_reports_whether_anything_was_stored() {
        let dir = tempfile::tempdir().unwrap();
        let store = store(&dir);
        let api = url("https://api.mergify.com");
        assert!(!store.delete(&api).unwrap());
        store.set(&api, &credential("mut_secret")).unwrap();
        assert!(store.delete(&api).unwrap());
        assert_eq!(store.get(&api).unwrap(), None);
    }

    // An emptied store leaves no file behind, so `{}` on disk never
    // has to be told apart from a credential that failed to load.
    #[test]
    fn deleting_the_last_credential_removes_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let store = store(&dir);
        let api = url("https://api.mergify.com");
        store.set(&api, &credential("mut_secret")).unwrap();
        assert!(store.file_path().unwrap().exists());
        store.delete(&api).unwrap();
        assert!(!store.file_path().unwrap().exists());
    }

    #[cfg(unix)]
    #[test]
    fn the_file_and_its_directory_are_owner_only() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let store = store(&dir);
        store
            .set(&url("https://api.mergify.com"), &credential("mut_secret"))
            .unwrap();

        let file_mode = fs::metadata(store.file_path().unwrap())
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(file_mode & 0o777, 0o600, "got {file_mode:o}");
        let dir_mode = fs::metadata(store.file_path().unwrap().parent().unwrap())
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(dir_mode & 0o777, 0o700, "got {dir_mode:o}");
    }

    // The state the lazy file path creates: no configuration
    // directory, and a keychain that did not take the credential
    // either. Reading is a clean "nothing stored"; writing has to
    // say why it cannot.
    #[test]
    fn a_store_with_no_backend_reads_empty_and_refuses_to_write() {
        let store = CredentialStore {
            use_keychain: false,
            file: None,
        };
        let api = url("https://api.mergify.com");
        assert_eq!(store.get(&api).unwrap(), None);
        assert!(!store.delete(&api).unwrap());

        let err = store.set(&api, &credential("mut_secret")).unwrap_err();
        assert!(
            err.to_string().contains("no configuration directory"),
            "got {err}",
        );
    }

    #[test]
    fn a_corrupt_store_is_an_error_not_an_empty_one() {
        let dir = tempfile::tempdir().unwrap();
        let store = store(&dir);
        fs::create_dir_all(store.file_path().unwrap().parent().unwrap()).unwrap();
        fs::write(store.file_path().unwrap(), b"{ this is not json").unwrap();

        let err = store.get(&url("https://api.mergify.com")).unwrap_err();
        let message = err.to_string();
        assert!(message.contains("credentials.json"), "got {message:?}");
        assert!(message.contains("auth login"), "got {message:?}");
    }
}
