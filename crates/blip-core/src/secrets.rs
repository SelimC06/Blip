//! API keys live in the OS credential store (macOS Keychain, Windows
//! Credential Manager), never in config.json.

use anyhow::Result;

const SERVICE: &str = "com.selim.blip";
const ANTHROPIC: &str = "anthropic_api_key";
const USAJOBS: &str = "usajobs_api_key";

fn entry(name: &str) -> Result<keyring::Entry> {
    Ok(keyring::Entry::new(SERVICE, name)?)
}

fn get(name: &str) -> Option<String> {
    entry(name).ok()?.get_password().ok().filter(|k| !k.trim().is_empty())
}

fn set(name: &str, key: &str) -> Result<()> {
    entry(name)?.set_password(key.trim())?;
    Ok(())
}

fn delete(name: &str) -> Result<()> {
    match entry(name)?.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(e.into()),
    }
}

/// Env var wins (handy for the CLI), then the credential store.
pub fn anthropic_key() -> Option<String> {
    if let Ok(k) = std::env::var("ANTHROPIC_API_KEY") {
        if !k.trim().is_empty() {
            return Some(k);
        }
    }
    get(ANTHROPIC)
}

pub fn has_stored_anthropic_key() -> bool {
    get(ANTHROPIC).is_some()
}

pub fn set_anthropic_key(key: &str) -> Result<()> {
    set(ANTHROPIC, key)
}

pub fn delete_anthropic_key() -> Result<()> {
    delete(ANTHROPIC)
}

/// Env var wins (handy for the CLI), then the credential store.
pub fn usajobs_key() -> Option<String> {
    if let Ok(k) = std::env::var("USAJOBS_API_KEY") {
        if !k.trim().is_empty() {
            return Some(k);
        }
    }
    get(USAJOBS)
}

pub fn set_usajobs_key(key: &str) -> Result<()> {
    set(USAJOBS, key)
}

pub fn delete_usajobs_key() -> Result<()> {
    delete(USAJOBS)
}
