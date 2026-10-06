//! Refresh tokens live only in the operating system credential store.
use tinytrac_drive::Secret;

pub trait Store: std::fmt::Debug + Send + Sync {
    fn load(&self, account: &str) -> Result<Option<Secret>, String>;
    fn save(&self, account: &str, token: &Secret) -> Result<(), String>;
    fn delete(&self, account: &str) -> Result<(), String>;
}

#[derive(Debug)]
pub struct Keychain;

#[cfg(any(target_os = "linux", target_os = "macos"))]
impl Keychain {
    fn entry(account: &str) -> Result<keyring::Entry, String> {
        keyring::Entry::new("net.expertstudio.tinytrac", account).map_err(keychain_error)
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn keychain_error(_: keyring::Error) -> String {
    "OS keychain unavailable; unlock your login keychain or GNOME Keyring and try again".into()
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
impl Store for Keychain {
    fn load(&self, account: &str) -> Result<Option<Secret>, String> {
        match Self::entry(account)?.get_password() {
            Ok(token) => Ok(Some(Secret::new(token))),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(error) => Err(keychain_error(error)),
        }
    }
    fn save(&self, account: &str, token: &Secret) -> Result<(), String> {
        Self::entry(account)?
            .set_password(token.expose())
            .map_err(keychain_error)
    }
    fn delete(&self, account: &str) -> Result<(), String> {
        match Self::entry(account)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(error) => Err(keychain_error(error)),
        }
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
impl Store for Keychain {
    fn load(&self, _: &str) -> Result<Option<Secret>, String> {
        Err("OS keychain unsupported".into())
    }
    fn save(&self, _: &str, _: &Secret) -> Result<(), String> {
        Err("OS keychain unsupported".into())
    }
    fn delete(&self, _: &str) -> Result<(), String> {
        Err("OS keychain unsupported".into())
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
    fn load(&self, account: &str) -> Result<Option<Secret>, String> {
        Ok(self.tokens.lock().unwrap().get(account).cloned())
    }
    fn save(&self, account: &str, token: &Secret) -> Result<(), String> {
        if self.fail.load(std::sync::atomic::Ordering::Relaxed) {
            return Err("Keychain locked".into());
        }
        self.tokens
            .lock()
            .unwrap()
            .insert(account.into(), token.clone());
        Ok(())
    }
    fn delete(&self, account: &str) -> Result<(), String> {
        self.tokens.lock().unwrap().remove(account);
        Ok(())
    }
}
