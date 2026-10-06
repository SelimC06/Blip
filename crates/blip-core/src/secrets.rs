//! API keys live in the OS credential store (macOS Keychain, Windows
//! Credential Manager), never in config.json.

use anyhow::Result;

const SERVICE: &str = "com.selim.blip";
const ANTHROPIC: &str = "anthropic_api_key";

fn entry() -> Result<keyring::Entry> {
    Ok(keyring::Entry::new(SERVICE, ANTHROPIC)?)
}

/// Env var wins (handy for the CLI), then the credential store.
pub fn anthropic_key() -> Option<String> {
    if let Ok(k) = std::env::var("ANTHROPIC_API_KEY") {
        if !k.trim().is_empty() {
            return Some(k);
        }
    }
    entry().ok()?.get_password().ok()
}

pub fn has_stored_anthropic_key() -> bool {
    entry().map(|e| e.get_password().is_ok()).unwrap_or(false)
}

pub fn set_anthropic_key(key: &str) -> Result<()> {
    entry()?.set_password(key.trim())?;
    Ok(())
}

pub fn delete_anthropic_key() -> Result<()> {
    match entry()?.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(e.into()),
    }
}
