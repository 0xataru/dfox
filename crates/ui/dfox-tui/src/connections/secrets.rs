//! Passwords, kept in the OS keychain: Keychain on macOS, Credential Manager
//! on Windows, the Secret Service on Linux.

#[cfg(test)]
use std::{collections::HashMap, sync::Mutex};

/// Where passwords are kept, by connection name.
pub trait Secrets {
    fn get(&self, name: &str) -> Result<Option<String>, String>;
    fn set(&self, name: &str, password: &str) -> Result<(), String>;
    fn delete(&self, name: &str) -> Result<(), String>;
}

/// The OS keychain, under the service name `dfox`.
pub struct Keychain;

const SERVICE: &str = "dfox";

impl Secrets for Keychain {
    fn get(&self, name: &str) -> Result<Option<String>, String> {
        let entry = keyring::Entry::new(SERVICE, name).map_err(|err| err.to_string())?;
        match entry.get_password() {
            Ok(password) => Ok(Some(password)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(err) => Err(err.to_string()),
        }
    }

    fn set(&self, name: &str, password: &str) -> Result<(), String> {
        keyring::Entry::new(SERVICE, name)
            .and_then(|entry| entry.set_password(password))
            .map_err(|err| err.to_string())
    }

    fn delete(&self, name: &str) -> Result<(), String> {
        let entry = keyring::Entry::new(SERVICE, name).map_err(|err| err.to_string())?;
        match entry.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(err) => Err(err.to_string()),
        }
    }
}

/// Passwords in memory, for tests.
#[cfg(test)]
#[derive(Default)]
pub struct Memory(Mutex<HashMap<String, String>>);

#[cfg(test)]
impl Secrets for Memory {
    fn get(&self, name: &str) -> Result<Option<String>, String> {
        Ok(self.lock().get(name).cloned())
    }

    fn set(&self, name: &str, password: &str) -> Result<(), String> {
        self.lock().insert(name.to_string(), password.to_string());
        Ok(())
    }

    fn delete(&self, name: &str) -> Result<(), String> {
        self.lock().remove(name);
        Ok(())
    }
}

#[cfg(test)]
impl Memory {
    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, String>> {
        self.0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// Moves a password to a connection's new name.
pub fn rename(secrets: &dyn Secrets, from: &str, to: &str) -> Result<(), String> {
    if from == to {
        return Ok(());
    }
    if let Some(password) = secrets.get(from)? {
        secrets.set(to, &password)?;
        secrets.delete(from)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renaming_moves_the_password() {
        let secrets = Memory::default();
        secrets.set("old", "pw").expect("set");
        rename(&secrets, "old", "new").expect("rename");
        assert_eq!(secrets.get("new").expect("get").as_deref(), Some("pw"));
        assert_eq!(secrets.get("old").expect("get"), None);
        rename(&secrets, "missing", "other").expect("nothing to move");
    }
}
