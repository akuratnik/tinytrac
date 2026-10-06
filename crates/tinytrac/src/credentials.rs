//! Refresh tokens live only in the operating system credential store.
use tinytrac_drive::Secret;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("OS keychain unavailable; unlock your login keychain or GNOME Keyring and try again")]
    Unavailable,
}

/// Install one native store before any workers or windows are started.
pub fn initialize() {
    #[cfg(target_os = "linux")]
    let store = zbus_secret_service_keyring_store::Store::new();
    #[cfg(target_os = "macos")]
    let store = apple_native_keyring_store::keychain::Store::new();
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    match store {
        Ok(store) => keyring_core::set_default_store(store),
        Err(error) => eprintln!("tinytrac: keychain: {error}"),
    }
}

pub trait Store: std::fmt::Debug + Send + Sync {
    fn load(&self, account: &str) -> Result<Option<Secret>, Error>;
    fn save(&self, account: &str, token: &Secret) -> Result<(), Error>;
    fn delete(&self, account: &str) -> Result<(), Error>;
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[derive(Debug)]
pub struct Keychain;

#[cfg(any(target_os = "linux", target_os = "macos"))]
impl Keychain {
    fn entry(account: &str) -> Result<keyring_core::Entry, Error> {
        keyring_core::Entry::new("net.expertstudio.tinytrac", account)
            .map_err(|error| keychain_error(&error))
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn keychain_error(error: &keyring_core::Error) -> Error {
    eprintln!("tinytrac: keychain: {error}");
    Error::Unavailable
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
impl Store for Keychain {
    fn load(&self, account: &str) -> Result<Option<Secret>, Error> {
        match Self::entry(account)?.get_password() {
            Ok(token) => Ok(Some(Secret::new(token))),
            Err(keyring_core::Error::NoEntry) => Ok(None),
            Err(error) => Err(keychain_error(&error)),
        }
    }
    fn save(&self, account: &str, token: &Secret) -> Result<(), Error> {
        Self::entry(account)?
            .set_password(token.expose())
            .map_err(|error| keychain_error(&error))
    }
    fn delete(&self, account: &str) -> Result<(), Error> {
        match Self::entry(account)?.delete_credential() {
            Ok(()) | Err(keyring_core::Error::NoEntry) => Ok(()),
            Err(error) => Err(keychain_error(&error)),
        }
    }
}

#[cfg(test)]
#[derive(Debug, Default)]
pub struct Memory {
    pub tokens: std::sync::Mutex<std::collections::BTreeMap<String, Secret>>,
    pub fail: std::sync::atomic::AtomicBool,
}
#[cfg(test)]
impl Store for Memory {
    fn load(&self, account: &str) -> Result<Option<Secret>, Error> {
        Ok(self.tokens.lock().unwrap().get(account).cloned())
    }
    fn save(&self, account: &str, token: &Secret) -> Result<(), Error> {
        if self.fail.load(std::sync::atomic::Ordering::Relaxed) {
            return Err(Error::Unavailable);
        }
        self.tokens
            .lock()
            .unwrap()
            .insert(account.into(), token.clone());
        Ok(())
    }
    fn delete(&self, account: &str) -> Result<(), Error> {
        self.tokens.lock().unwrap().remove(account);
        Ok(())
    }
}
