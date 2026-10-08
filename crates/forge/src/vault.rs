//! Where the sign-in is kept: the system's credential store (the macOS Keychain, the Windows Credential Manager).
//! It is never written to a file of ours. Where there is no such store the sign-in lasts until the program ends.

use crate::token::Token;

#[cfg(any(target_os = "macos", target_os = "windows"))]
mod store {
    use super::Token;
    use keyring::{Entry, Error};

    fn entry() -> Result<Entry, String> {
        Entry::new("gitgui", "github").map_err(|err| err.to_string())
    }

    pub fn load() -> Result<Option<Token>, String> {
        match entry()?.get_password() {
            Ok(text) => Ok(serde_json::from_str(&text).ok()),
            Err(Error::NoEntry) => Ok(None),
            Err(err) => Err(err.to_string()),
        }
    }

    pub fn save(token: &Token) -> Result<(), String> {
        let text = serde_json::to_string(token).map_err(|err| err.to_string())?;
        entry()?.set_password(&text).map_err(|err| err.to_string())
    }

    pub fn delete() -> Result<(), String> {
        match entry()?.delete_credential() {
            Ok(()) | Err(Error::NoEntry) => Ok(()),
            Err(err) => Err(err.to_string()),
        }
    }
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
mod store {
    use super::Token;

    pub fn load() -> Result<Option<Token>, String> {
        Ok(None)
    }

    pub fn save(_: &Token) -> Result<(), String> {
        Err("this system has no credential store gitgui can use".to_owned())
    }

    pub fn delete() -> Result<(), String> {
        Ok(())
    }
}

/// The saved sign-in, if there is one.
pub fn load() -> Result<Option<Token>, String> {
    store::load()
}

pub fn save(token: &Token) -> Result<(), String> {
    store::save(token)
}

/// Forgets the sign-in (Sign out).
pub fn delete() -> Result<(), String> {
    store::delete()
}
