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
//!    there — so a machine with no credential store falls back
//!    silently.
//!
//! A keychain that *exists* and refuses is the one fatal case. `get`
//! reads the keychain before the file, so an entry left in it would
//! shadow whatever the fallback stored: `set` and `delete` verify
//! the keychain no longer holds the key, and refuse rather than
//! leave the user on a credential they think they replaced. "Locked"
//! and "empty" are told apart by [`KeychainState`], because a
//! read-back that cannot distinguish them verifies nothing.
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
#[derive(Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct Credential {
    pub token: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<DateTime<Utc>>,
}

/// Hand-written, because a derived one prints the token. Every
/// `Debug` in this module exists to trace *where* a credential came
/// from, and the first `?credential` in a `tracing` call would have
/// put the secret itself in a log file.
impl std::fmt::Debug for Credential {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Credential")
            .field("token", &"<redacted>")
            .field("expires_at", &self.expires_at)
            .finish()
    }
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
    /// [`KeychainBackend::Off`] skips the keychain entirely, so a
    /// unit test never reaches — or worse, writes to — the
    /// developer's real keychain.
    keychain: KeychainBackend,
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
            keychain: KeychainBackend::Os,
            file: credentials_file(),
        }
    }

    /// A keychain-free store whose file lives directly at `path`.
    /// Tests use it; so does any caller that has already decided
    /// the keychain is not an option.
    #[must_use]
    pub fn file_at(path: PathBuf) -> Self {
        Self {
            keychain: KeychainBackend::Off,
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
        if let Some(raw) = self.keychain.get(&key) {
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
        if self.keychain.enabled() && self.keychain.set(&key, &secret) {
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
        // The keychain refused the write, so the fallback has to
        // take it. Everything that can refuse the fallback is
        // resolved *before* the stale keychain entry is deleted: a
        // delete followed by a failed write would leave the machine
        // with no credential at all, and `login` revokes the token
        // it just minted on this error path — so the user would end
        // up logged out of a machine that was working a second ago.
        let Some(file) = &self.file else {
            return Err(CliError::Configuration(
                "no system keychain answered, and this machine offers no configuration \
                 directory to store the credential in — check that your user has a home \
                 directory"
                    .to_string(),
            ));
        };
        let mut entries = read_file(file)?;
        // If the keychain is nevertheless holding an older entry for
        // this URL, writing the fallback would store a credential
        // that can never be read: `get` checks the keychain first and
        // would keep answering with the stale one. So read back
        // rather than trust the delete, and refuse unless that
        // read-back is conclusive: "the store refused" is not "there
        // is nothing there".
        if self.keychain.enabled() {
            self.keychain.delete(&key);
            match self.keychain.probe(&key) {
                KeychainState::Absent => {}
                KeychainState::Present => {
                    return Err(CliError::Configuration(format!(
                        "an older credential for {api_url} is in the system keychain and \
                         could not be replaced or removed. Unlock your keychain and try \
                         again.",
                    )));
                }
                KeychainState::Unknown => {
                    return Err(CliError::Configuration(format!(
                        "the system keychain refused both the write and a read-back, so an \
                         older credential for {api_url} may still be in it — and one there \
                         would shadow anything stored on disk. Unlock your keychain and try \
                         again.",
                    )));
                }
            }
        }
        entries.insert(key.clone(), credential.clone());
        write_file(file, &entries)?;
        Ok(Location::File(file.clone()))
    }

    /// Forget the credential for `api_url` in both backends.
    /// Returns whether either of them held one.
    ///
    /// # Errors
    ///
    /// [`CliError::Configuration`] when the keychain still holds
    /// the credential afterwards, or will not say. A keychain locked
    /// against a delete reports the same `false` as one that had
    /// nothing, so the entry is read back rather than the delete
    /// believed — and a read-back that cannot answer is an error
    /// too, because `logout` promises a machine that no longer
    /// carries the credential.
    pub fn delete(&self, api_url: &Url) -> Result<bool, CliError> {
        let key = key_for(api_url);
        // The keychain first, and its verdict held back until the
        // file has been tried too. The two backends fail
        // independently: a corrupt `credentials.json` used to abort
        // the whole call before the keychain was touched, which made
        // `logout` a permanent no-op against the one backend that
        // actually held the credential.
        let mut from_keychain = false;
        let mut keychain_verdict = Ok(());
        if self.keychain.enabled() {
            from_keychain = self.keychain.delete(&key);
            keychain_verdict = match self.keychain.probe(&key) {
                KeychainState::Absent => Ok(()),
                KeychainState::Present => Err(CliError::Configuration(format!(
                    "the credential for {api_url} is still in the system keychain: it could \
                     not be removed. Unlock your keychain and run `mergify auth logout` \
                     again.",
                ))),
                KeychainState::Unknown => Err(CliError::Configuration(format!(
                    "the system keychain refused a read-back, so the credential for \
                     {api_url} may still be in it. Unlock your keychain and run `mergify \
                     auth logout` again.",
                ))),
            };
        }
        let from_file = self.remove_from_file(&key);
        // The keychain's verdict first: a credential still live in it
        // outranks a file this call could not rewrite. Then the
        // file's own error -- not folded into the `||` below, where
        // short-circuiting on a successful keychain delete would
        // swallow it.
        keychain_verdict?;
        let from_file = from_file?;
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
    // Each write is atomic (temp file + rename) but the surrounding
    // read-modify-write is not locked, so two `auth` commands racing
    // on *different* API URLs can lose one entry. Not fixed here: the
    // writers are `auth login` and `auth logout`, both driven by a
    // person at a terminal, and a lock file is its own design — stale
    // locks, no-home machines, Windows semantics. It is a real hole,
    // just a narrow one.
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
/// is not a different deployment. The HTTP client gives its base URL
/// a trailing slash and strips the request path's leading one before
/// joining, so `https://host/api` and `https://host/api/` both send
/// `/v1/user` to `https://host/api/v1/user`. Two entries for them
/// would tell a user who logged in with one spelling that they are
/// not logged in with the other.
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

/// What the keychain says about one entry, when the answer has to
/// be trusted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum KeychainState {
    /// The entry is there.
    Present,
    /// Nothing is there to read — either the store said so, or this
    /// machine has no credential store at all, which for a caller
    /// asking "could an entry here shadow the file?" is the same
    /// answer.
    Absent,
    /// A store exists and refused to answer: locked, or a user who
    /// clicked Deny. Nothing can be concluded from that.
    Unknown,
}

/// The keychain half of the store.
///
/// An enum rather than four free functions because the branches that
/// decide whether a failed `login` is *fatal* are the delicate part
/// of this module, and they are unreachable in a test that cannot
/// stand a keychain up. `Fake` is the seam; it exists only under
/// `cfg(test)`, so the shipped binary has the two real arms.
enum KeychainBackend {
    /// The platform credential store, via `keyring`.
    Os,
    /// No keychain: what a file-only caller asks for.
    Off,
    #[cfg(any(test, feature = "test-support"))]
    Fake(std::sync::Mutex<FakeKeychain>),
}

impl KeychainBackend {
    /// Whether the keychain is consulted at all. `false` skips every
    /// keychain leg, including the verification ones.
    fn enabled(&self) -> bool {
        !matches!(self, Self::Off)
    }

    /// Read one entry: `Ok(None)` is "nothing filed here", `Err` is
    /// the store declining to answer.
    fn read(&self, key: &str) -> Result<Option<String>, keyring::Error> {
        match self {
            Self::Os => {
                match keyring::Entry::new(KEYRING_SERVICE, key).and_then(|e| e.get_password()) {
                    Ok(secret) => Ok(Some(secret)),
                    Err(keyring::Error::NoEntry) => Ok(None),
                    Err(e) => Err(e),
                }
            }
            // Consistently "there is no store here", so `get`
            // answers `None`, `probe` answers `Absent`, and the two
            // verification branches cannot fire on a file-only store.
            Self::Off => Err(keyring::Error::NoDefaultStore),
            #[cfg(any(test, feature = "test-support"))]
            Self::Fake(fake) => fake.lock().unwrap().read(),
        }
    }

    fn write(&self, key: &str, secret: &str) -> Result<(), keyring::Error> {
        match self {
            Self::Os => {
                keyring::Entry::new(KEYRING_SERVICE, key).and_then(|e| e.set_password(secret))
            }
            Self::Off => Err(keyring::Error::NoDefaultStore),
            #[cfg(any(test, feature = "test-support"))]
            Self::Fake(fake) => fake.lock().unwrap().write(secret),
        }
    }

    /// Remove one entry, reporting whether one was there.
    fn remove(&self, key: &str) -> Result<bool, keyring::Error> {
        match self {
            Self::Os => match keyring::Entry::new(KEYRING_SERVICE, key)
                .and_then(|e| e.delete_credential())
            {
                Ok(()) => Ok(true),
                Err(keyring::Error::NoEntry) => Ok(false),
                Err(e) => Err(e),
            },
            Self::Off => Err(keyring::Error::NoDefaultStore),
            #[cfg(any(test, feature = "test-support"))]
            Self::Fake(fake) => fake.lock().unwrap().remove(),
        }
    }

    /// Read one entry for the *read path*. Every failure — no entry,
    /// no keychain, a locked one, a user who clicked Deny —
    /// collapses to `None`: the caller's next move is the file
    /// backend either way, and the distinction is only worth a debug
    /// line.
    fn get(&self, key: &str) -> Option<String> {
        match self.read(key) {
            Ok(secret) => secret,
            Err(e) => {
                tracing::debug!(error = %e, "no credential read from the system keychain");
                None
            }
        }
    }

    /// Write one entry, reporting whether it took. `false` means the
    /// caller falls back to the file.
    fn set(&self, key: &str, secret: &str) -> bool {
        match self.write(key, secret) {
            Ok(()) => true,
            Err(e) => {
                tracing::debug!(error = %e, "could not store the credential in the system keychain");
                false
            }
        }
    }

    /// Delete one entry, reporting whether one was there. A store
    /// that refused reports the same `false` as an empty one, which
    /// is why every caller follows this with [`Self::probe`].
    fn delete(&self, key: &str) -> bool {
        match self.remove(key) {
            Ok(removed) => removed,
            Err(e) => {
                tracing::debug!(
                    error = %e,
                    "could not delete the credential from the system keychain"
                );
                false
            }
        }
    }

    /// Ask about `key`, keeping the three answers apart.
    ///
    /// [`Self::get`] collapses every failure to `None` because its
    /// caller reaches for the file either way. The two verification
    /// points cannot afford that: they are deciding whether an entry
    /// `get` reads *first* is about to shadow what they just wrote,
    /// and "the store refused" is not "there is nothing there".
    fn probe(&self, key: &str) -> KeychainState {
        match self.read(key) {
            Ok(Some(_)) => KeychainState::Present,
            Ok(None) => KeychainState::Absent,
            Err(e) => keychain_state_for(&e),
        }
    }
}

/// Split out from [`KeychainBackend::probe`] so the mapping that
/// decides whether a failed login is fatal is testable on its own.
fn keychain_state_for(error: &keyring::Error) -> KeychainState {
    match error {
        // `NoEntry` is the store answering "nothing filed here".
        //
        // `NoDefaultStore` is `keyring` failing to *initialize* a
        // store, which it caches process-wide in a `LazyLock`. That
        // covers both "this machine has no credential store" — a
        // container, a systemd unit, an SSH session with no D-Bus,
        // the case the whole file fallback exists for — and "the
        // store is there but could not be reached just now". The
        // crate does not tell the two apart (`Entry::store_status()`
        // separates only an unsupported platform), and they want
        // opposite answers, so this picks the one whose failure is
        // recoverable: treating it as `Unknown` would make
        // `auth login` fatal on every headless machine, while
        // treating it as `Absent` costs a desktop whose session bus
        // blinked one stale-credential 403 that the next
        // `auth login` clears.
        keyring::Error::NoEntry | keyring::Error::NoDefaultStore => KeychainState::Absent,
        _ => {
            tracing::debug!(error = %error, "could not verify the system keychain entry");
            KeychainState::Unknown
        }
    }
}

#[cfg(any(test, feature = "test-support"))]
impl CredentialStore {
    /// A store whose keychain is [`FakeKeychain`], so the branches
    /// that decide whether a login is fatal can be exercised.
    fn with_fake(fake: FakeKeychain, file: Option<PathBuf>) -> Self {
        Self {
            keychain: KeychainBackend::Fake(std::sync::Mutex::new(fake)),
            file,
        }
    }
}

#[cfg(test)]
impl CredentialStore {
    /// What the fake keychain is holding, for a test that has to
    /// assert an entry survived — or did not.
    fn fake_entry(&self) -> Option<String> {
        match &self.keychain {
            KeychainBackend::Fake(fake) => fake.lock().unwrap().entry.clone(),
            _ => panic!("not a fake keychain"),
        }
    }
}

/// Stores that model one specific way a real keychain misbehaves,
/// for a **caller's** tests — `auth logout`'s "removed but never
/// read" branch is unreachable with a keychain-free store, and
/// unreachable code is code an edit can break with the suite green.
///
/// Behind the `test-support` feature, which nothing but a
/// dev-dependency turns on.
#[cfg(feature = "test-support")]
impl CredentialStore {
    /// A store holding `secret` in a keychain that refuses every
    /// *read* while still deleting on request. That is macOS with a
    /// denied read ACL: deleting an item needs no decrypt, so
    /// `logout` removes a credential it never saw and has nothing to
    /// revoke.
    #[must_use]
    pub fn with_unreadable_keychain(secret: &str) -> Self {
        Self::with_fake(
            FakeKeychain {
                entry: Some(secret.to_string()),
                refuse_reads: true,
                ..FakeKeychain::default()
            },
            None,
        )
    }
}

/// A keychain that can be locked, denied, or emptied on demand.
///
/// Models the three ways a real one misbehaves independently,
/// because they are independent in practice: macOS will happily
/// delete an item whose read ACL it refuses (deleting needs no
/// decrypt), and a locked store refuses reads and writes while
/// still existing.
#[cfg(any(test, feature = "test-support"))]
#[derive(Default)]
struct FakeKeychain {
    entry: Option<String>,
    refuse_reads: bool,
    refuse_writes: bool,
    refuse_deletes: bool,
}

#[cfg(any(test, feature = "test-support"))]
impl FakeKeychain {
    fn locked(error: &str) -> keyring::Error {
        keyring::Error::NoStorageAccess(error.into())
    }

    fn read(&self) -> Result<Option<String>, keyring::Error> {
        if self.refuse_reads {
            return Err(Self::locked("the keychain refused the read"));
        }
        Ok(self.entry.clone())
    }

    fn write(&mut self, secret: &str) -> Result<(), keyring::Error> {
        if self.refuse_writes {
            return Err(Self::locked("the keychain refused the write"));
        }
        self.entry = Some(secret.to_string());
        Ok(())
    }

    fn remove(&mut self) -> Result<bool, keyring::Error> {
        if self.refuse_deletes {
            return Err(Self::locked("the keychain refused the delete"));
        }
        Ok(self.entry.take().is_some())
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
            keychain: KeychainBackend::Off,
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

    #[test]
    fn debug_never_prints_the_token() {
        let stored = StoredCredential {
            credential: credential("mut_secret"),
            location: Location::Keychain,
        };

        // Both shapes, because `StoredCredential` derives `Debug`
        // and would print the token through the inner one.
        let rendered = format!("{:?} {:?}", stored.credential, stored);
        assert!(
            !rendered.contains("mut_secret"),
            "the token reached debug output: {rendered}",
        );
        assert!(rendered.contains("<redacted>"), "got {rendered}");
    }

    #[test]
    fn a_machine_with_no_credential_store_reads_as_absent() {
        // The container / headless-SSH case. Classified `Absent` so
        // the file fallback stays non-fatal: there is no store to
        // hold an entry that could shadow it.
        assert_eq!(
            keychain_state_for(&keyring::Error::NoDefaultStore),
            KeychainState::Absent,
        );
        assert_eq!(
            keychain_state_for(&keyring::Error::NoEntry),
            KeychainState::Absent,
        );
    }

    #[test]
    fn a_keychain_that_refuses_to_answer_is_not_read_as_empty() {
        // The bug this mapping exists for: a locked keychain, or a
        // user who clicked Deny, answers with an error — and a
        // read-back that called that "empty" would report a
        // credential removed while it is still there.
        for error in [
            keyring::Error::NoStorageAccess("locked".into()),
            keyring::Error::PlatformFailure("dbus went away".into()),
        ] {
            assert_eq!(keychain_state_for(&error), KeychainState::Unknown);
        }
    }

    /// The stored form of a credential, as the keychain holds it.
    fn secret(token: &str) -> String {
        serde_json::to_string(&credential(token)).unwrap()
    }

    #[test]
    fn a_refused_write_over_a_stale_entry_refuses_the_login() {
        let dir = tempfile::tempdir().unwrap();
        let store = CredentialStore::with_fake(
            FakeKeychain {
                entry: Some(secret("mut_old")),
                refuse_writes: true,
                refuse_deletes: true,
                ..FakeKeychain::default()
            },
            Some(dir.path().join("mergify").join("credentials.json")),
        );
        let api = url("https://api.mergify.com");

        let err = store.set(&api, &credential("mut_new")).unwrap_err();
        assert!(err.to_string().contains("older credential"), "got {err}");
        // Nothing may reach the file: `get` reads the keychain first,
        // so a file copy written here could never be read.
        assert!(!store.file_path().unwrap().exists());
    }

    #[test]
    fn a_refused_write_with_an_unreadable_keychain_refuses_the_login() {
        let dir = tempfile::tempdir().unwrap();
        let store = CredentialStore::with_fake(
            FakeKeychain {
                entry: Some(secret("mut_old")),
                refuse_reads: true,
                refuse_writes: true,
                refuse_deletes: true,
            },
            Some(dir.path().join("mergify").join("credentials.json")),
        );

        let err = store
            .set(&url("https://api.mergify.com"), &credential("mut_new"))
            .unwrap_err();
        assert!(err.to_string().contains("read-back"), "got {err}");
    }

    #[test]
    fn a_refused_write_keeps_the_keychain_entry_when_there_is_nowhere_to_fall_back() {
        // The delete used to run before the fallback was known to be
        // available, so this machine ended up with no credential at
        // all -- and `login` revokes the new token on this error.
        let store = CredentialStore::with_fake(
            FakeKeychain {
                entry: Some(secret("mut_old")),
                refuse_writes: true,
                ..FakeKeychain::default()
            },
            None,
        );

        let err = store
            .set(&url("https://api.mergify.com"), &credential("mut_new"))
            .unwrap_err();
        assert!(
            err.to_string().contains("no configuration directory"),
            "got {err}",
        );
        assert_eq!(store.fake_entry(), Some(secret("mut_old")));
    }

    #[test]
    fn logout_clears_the_keychain_even_when_the_credential_file_is_corrupt() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("mergify").join("credentials.json");
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(&file, b"{ this is not json").unwrap();
        let store = CredentialStore::with_fake(
            FakeKeychain {
                entry: Some(secret("mut_stored")),
                ..FakeKeychain::default()
            },
            Some(file),
        );

        // The file leg still reports its own failure...
        let err = store.delete(&url("https://api.mergify.com")).unwrap_err();
        assert!(err.to_string().contains("credentials.json"), "got {err}");
        // ...but the keychain, which is where the credential actually
        // was, has been cleared rather than skipped.
        assert_eq!(store.fake_entry(), None);
    }

    #[test]
    fn a_keychain_write_clears_the_file_copy() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("mergify").join("credentials.json");
        let api = url("https://api.mergify.com");
        CredentialStore::file_at(file.clone())
            .set(&api, &credential("mut_on_disk"))
            .unwrap();

        let store = CredentialStore::with_fake(FakeKeychain::default(), Some(file.clone()));
        assert_eq!(
            store.set(&api, &credential("mut_new")).unwrap(),
            Location::Keychain
        );

        assert_eq!(store.fake_entry(), Some(secret("mut_new")));
        // The emptied store is removed outright, so there is no
        // readable copy left behind on disk.
        assert!(!file.exists());
    }
}
