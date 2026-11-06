//! Connection passwords in the macOS login Keychain (generic passwords,
//! service "RDP Launcher", account = connection id).

use std::fmt;

use security_framework::passwords::{delete_generic_password, get_generic_password, set_generic_password};

const APP_SERVICE: &str = "RDP Launcher";

#[derive(Debug)]
pub struct KeychainError(pub String);

impl fmt::Display for KeychainError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Keychain error: {}", self.0)
    }
}

impl std::error::Error for KeychainError {}

pub struct Keychain {
    service: String,
}

impl Keychain {
    pub fn new(service: &str) -> Self {
        Keychain { service: service.to_owned() }
    }

    pub fn app() -> Self {
        Self::new(APP_SERVICE)
    }

    pub fn get(&self, id: &str) -> Option<String> {
        get_generic_password(&self.service, id)
            .ok()
            .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
    }

    /// Store `password`; an empty password removes the entry.
    pub fn set(&self, id: &str, password: &str) -> Result<(), KeychainError> {
        if password.is_empty() {
            self.delete(id);
            return Ok(());
        }
        set_generic_password(&self.service, id, password.as_bytes()).map_err(|e| KeychainError(e.to_string()))
    }

    pub fn delete(&self, id: &str) {
        let _ = delete_generic_password(&self.service, id);
    }

    /// Give connection `to` the same password as `from` (no-op if `from` has none).
    pub fn copy(&self, from: &str, to: &str) -> Result<(), KeychainError> {
        match self.get(from) {
            Some(password) => self.set(to, &password),
            None => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kc() -> Keychain {
        Keychain::new("RDP Launcher Tests")
    }

    fn id() -> String {
        uuid::Uuid::new_v4().to_string()
    }

    #[test]
    fn set_get_delete_round_trip() {
        let (k, a) = (kc(), id());
        k.set(&a, "p@ss wörd").unwrap();
        assert_eq!(k.get(&a).as_deref(), Some("p@ss wörd"));
        k.delete(&a);
        assert_eq!(k.get(&a), None);
    }

    #[test]
    fn set_overwrites_existing() {
        let (k, a) = (kc(), id());
        k.set(&a, "one").unwrap();
        k.set(&a, "two").unwrap();
        assert_eq!(k.get(&a).as_deref(), Some("two"));
        k.delete(&a);
    }

    #[test]
    fn set_empty_deletes() {
        let (k, a) = (kc(), id());
        k.set(&a, "x").unwrap();
        k.set(&a, "").unwrap();
        assert_eq!(k.get(&a), None);
    }

    #[test]
    fn copy_duplicates_entry() {
        let (k, a, b) = (kc(), id(), id());
        k.set(&a, "secret").unwrap();
        k.copy(&a, &b).unwrap();
        assert_eq!(k.get(&b).as_deref(), Some("secret"));
        k.delete(&a);
        k.delete(&b);
    }

    #[test]
    fn copy_of_missing_is_noop() {
        let (k, a, b) = (kc(), id(), id());
        k.copy(&a, &b).unwrap();
        assert_eq!(k.get(&b), None);
    }

    #[test]
    fn get_missing_is_none() {
        assert_eq!(kc().get(&id()), None);
    }
}
