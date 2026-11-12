//! Connection passwords in the macOS login Keychain (generic passwords,
//! service "RDP Launcher", account = connection id).

use std::fmt;

use security_framework::passwords::{delete_generic_password, get_generic_password, set_generic_password};

const APP_SERVICE: &str = "RDP Launcher";
// Security.framework's errSecItemNotFound. Other errors (locked Keychain,
// denied access, etc.) must not be treated as a missing saved password.
const ITEM_NOT_FOUND: i32 = -25300;

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

    pub fn get(&self, id: &str) -> Result<Option<String>, KeychainError> {
        decode_password(get_generic_password(&self.service, id))
    }

    /// Store `password`; an empty password removes the entry.
    pub fn set(&self, id: &str, password: &str) -> Result<(), KeychainError> {
        if password.is_empty() {
            return self.delete(id);
        }
        set_generic_password(&self.service, id, password.as_bytes()).map_err(|e| KeychainError(e.to_string()))
    }

    pub fn delete(&self, id: &str) -> Result<(), KeychainError> {
        match delete_generic_password(&self.service, id) {
            Ok(()) => Ok(()),
            Err(e) if e.code() == ITEM_NOT_FOUND => Ok(()),
            Err(e) => Err(KeychainError(e.to_string())),
        }
    }

    /// Give connection `to` the same password as `from` (no-op if `from` has none).
    pub fn copy(&self, from: &str, to: &str) -> Result<(), KeychainError> {
        match self.get(from)? {
            Some(password) => self.set(to, &password),
            None => Ok(()),
        }
    }
}

fn decode_password(result: Result<Vec<u8>, security_framework::base::Error>) -> Result<Option<String>, KeychainError> {
    match result {
        Ok(bytes) => String::from_utf8(bytes)
            .map(Some)
            .map_err(|_| KeychainError("Saved password is not valid UTF-8; re-enter it in Edit Connection.".into())),
        Err(e) if e.code() == ITEM_NOT_FOUND => Ok(None),
        Err(e) => Err(KeychainError(e.to_string())),
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
        k.set(&a, " p@ss wörd 🔑 ").unwrap();
        assert_eq!(k.get(&a).unwrap().as_deref(), Some(" p@ss wörd 🔑 "));
        k.delete(&a).unwrap();
        assert_eq!(k.get(&a).unwrap(), None);
    }

    #[test]
    fn set_overwrites_existing() {
        let (k, a) = (kc(), id());
        k.set(&a, "one").unwrap();
        k.set(&a, "two").unwrap();
        assert_eq!(k.get(&a).unwrap().as_deref(), Some("two"));
        k.delete(&a).unwrap();
    }

    #[test]
    fn set_empty_deletes() {
        let (k, a) = (kc(), id());
        k.set(&a, "x").unwrap();
        k.set(&a, "").unwrap();
        assert_eq!(k.get(&a).unwrap(), None);
    }

    #[test]
    fn copy_duplicates_entry() {
        let (k, a, b) = (kc(), id(), id());
        k.set(&a, "secret").unwrap();
        k.copy(&a, &b).unwrap();
        assert_eq!(k.get(&b).unwrap().as_deref(), Some("secret"));
        k.delete(&a).unwrap();
        k.delete(&b).unwrap();
    }

    #[test]
    fn copy_of_missing_is_noop() {
        let (k, a, b) = (kc(), id(), id());
        k.copy(&a, &b).unwrap();
        assert_eq!(k.get(&b).unwrap(), None);
    }

    #[test]
    fn get_missing_is_none() {
        assert_eq!(kc().get(&id()).unwrap(), None);
    }

    #[test]
    fn keychain_access_errors_are_not_missing_passwords() {
        use security_framework::base::Error;

        assert_eq!(decode_password(Err(Error::from_code(ITEM_NOT_FOUND))).unwrap(), None);
        // errSecAuthFailed and errSecInteractionNotAllowed.
        for code in [-25293, -25308] {
            assert!(decode_password(Err(Error::from_code(code))).is_err());
        }
    }

    #[test]
    fn invalid_password_encoding_is_reported_without_exposing_bytes() {
        let error = decode_password(Ok(b"secret\xff".to_vec())).unwrap_err();
        assert!(error.to_string().contains("UTF-8"));
        assert!(!error.to_string().contains("secret"));
    }
}
