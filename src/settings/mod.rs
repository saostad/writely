#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "windows")]
mod windows;

const SERVICE: &str = "writely";
const USER: &str = "gemini-api-key";
const TOGETHER_USER: &str = "together-api-key";
pub(crate) const API_KEY_PAGE: &str = "https://aistudio.google.com/apikey";
pub(crate) const TOGETHER_API_KEY_PAGE: &str = "https://api.together.ai/settings/api-keys";
pub(crate) const ACCESSIBILITY_SETTINGS_URL: &str =
    "x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility";

/// The keychain page and storage for the provider that needs a key.
pub fn api_key_page_for(provider: crate::config::Provider) -> Option<&'static str> {
    match provider {
        crate::config::Provider::Gemini => Some(API_KEY_PAGE),
        crate::config::Provider::Together => Some(TOGETHER_API_KEY_PAGE),
        crate::config::Provider::Ollama => None,
    }
}

fn keychain_user_for(provider: crate::config::Provider) -> Option<&'static str> {
    match provider {
        crate::config::Provider::Gemini => Some(USER),
        crate::config::Provider::Together => Some(TOGETHER_USER),
        crate::config::Provider::Ollama => None,
    }
}

static PERMISSION_MESSAGE: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);
static HOTKEY_MESSAGE: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

pub fn set_hotkey_message(message: Option<String>) {
    match HOTKEY_MESSAGE.lock() {
        Ok(mut guard) => *guard = message,
        Err(poisoned) => *poisoned.into_inner() = message,
    }
}

pub fn hotkey_message() -> Option<String> {
    match HOTKEY_MESSAGE.lock() {
        Ok(guard) => guard.clone(),
        Err(poisoned) => poisoned.into_inner().clone(),
    }
}

pub fn set_permission_message(message: Option<String>) {
    match PERMISSION_MESSAGE.lock() {
        Ok(mut guard) => *guard = message,
        Err(poisoned) => *poisoned.into_inner() = message,
    }
}

pub fn permission_message() -> Option<String> {
    match PERMISSION_MESSAGE.lock() {
        Ok(guard) => guard.clone(),
        Err(poisoned) => poisoned.into_inner().clone(),
    }
}

/// Refresh an open Settings window after Accessibility permission is granted.
/// Returns true if a window was open and refreshed.
#[allow(dead_code)]
pub fn notify_permission_granted() -> bool {
    #[cfg(target_os = "macos")]
    {
        macos::notify_permission_granted()
    }
    #[cfg(not(target_os = "macos"))]
    {
        false
    }
}

pub fn validate_api_key(key: &str) -> Result<String, String> {
    let trimmed = key.trim();
    if trimmed.is_empty() {
        Err("Enter an API key before saving.".to_string())
    } else {
        Ok(trimmed.to_string())
    }
}

fn keyring_entry() -> Result<keyring::Entry, keyring::Error> {
    keyring::Entry::new(SERVICE, USER)
}

fn keyring_entry_for(provider: crate::config::Provider) -> Result<keyring::Entry, keyring::Error> {
    let user = keychain_user_for(provider).unwrap_or(USER);
    keyring::Entry::new(SERVICE, user)
}

/// Loads the API key for the selected provider. Ollama has no key (`Ok(None)`).
pub fn load_api_key_for(provider: crate::config::Provider) -> Result<Option<String>, String> {
    if keychain_user_for(provider).is_none() {
        return Ok(None);
    }
    let entry = keyring_entry_for(provider)
        .map_err(|e| format!("Could not read the key from the system keychain: {e}"))?;
    match entry.get_password() {
        Ok(key) => Ok(Some(key)),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(e) => Err(format!(
            "Could not read the key from the system keychain: {e}"
        )),
    }
}

/// Saves the API key for the selected provider. Ollama has no key (a no-op).
pub fn save_api_key_for(provider: crate::config::Provider, key: &str) -> Result<(), String> {
    if keychain_user_for(provider).is_none() {
        return Ok(());
    }
    let trimmed = validate_api_key(key)?;
    let entry = keyring_entry_for(provider)
        .map_err(|e| format!("Could not save the key to the system keychain: {e}"))?;
    entry
        .set_password(&trimmed)
        .map_err(|e| format!("Could not save the key to the system keychain: {e}"))
}

/// Removes the selected provider's key. Ollama has no key (a no-op).
pub fn remove_api_key_for(provider: crate::config::Provider) -> Result<(), String> {
    if keychain_user_for(provider).is_none() {
        return Ok(());
    }
    let entry = keyring_entry_for(provider)
        .map_err(|e| format!("Could not remove the key from the system keychain: {e}"))?;
    match entry.delete_credential() {
        Ok(()) => Ok(()),
        Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(format!(
            "Could not remove the key from the system keychain: {e}"
        )),
    }
}

pub fn has_api_key_for(provider: crate::config::Provider) -> Result<bool, String> {
    load_api_key_for(provider).map(|key| key.is_some())
}

pub fn load_api_key() -> Result<Option<String>, String> {
    let entry = keyring_entry()
        .map_err(|e| format!("Could not read the key from the system keychain: {e}"))?;
    match entry.get_password() {
        Ok(key) => Ok(Some(key)),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(e) => Err(format!(
            "Could not read the key from the system keychain: {e}"
        )),
    }
}

// Kept for the Windows/Linux Settings windows (unused on macOS).
#[allow(dead_code)]
pub fn save_api_key(key: &str) -> Result<(), String> {
    let trimmed = validate_api_key(key)?;
    let entry = keyring_entry()
        .map_err(|e| format!("Could not save the key to the system keychain: {e}"))?;
    entry
        .set_password(&trimmed)
        .map_err(|e| format!("Could not save the key to the system keychain: {e}"))
}

// Kept for the Windows/Linux Settings windows (unused on macOS).
#[allow(dead_code)]
pub fn remove_api_key() -> Result<(), String> {
    let entry = keyring_entry()
        .map_err(|e| format!("Could not remove the key from the system keychain: {e}"))?;
    match entry.delete_credential() {
        Ok(()) => Ok(()),
        Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(format!(
            "Could not remove the key from the system keychain: {e}"
        )),
    }
}

// Kept for the Windows/Linux Settings windows (unused on macOS).
#[allow(dead_code)]
pub fn has_api_key() -> Result<bool, String> {
    load_api_key().map(|key| key.is_some())
}

#[allow(dead_code)]
pub fn is_settings_key_window() -> bool {
    #[cfg(target_os = "macos")]
    {
        macos::is_settings_key_window()
    }
    #[cfg(not(target_os = "macos"))]
    {
        false
    }
}

pub fn show_settings() -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        macos::show_settings()
    }
    #[cfg(target_os = "windows")]
    {
        windows::show_settings()
    }
    #[cfg(target_os = "linux")]
    {
        linux::show_settings()
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
    {
        Err("Could not open the settings window: this platform is not supported.".to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::validate_api_key;

    #[test]
    fn trims_surrounding_whitespace() {
        assert_eq!(
            validate_api_key("  abc123  ").expect("key should be valid"),
            "abc123"
        );
    }

    #[test]
    fn trims_tabs_and_newlines() {
        assert_eq!(
            validate_api_key("\t abc123 \n").expect("key should be valid"),
            "abc123"
        );
    }

    #[test]
    fn rejects_empty_key() {
        assert_eq!(
            validate_api_key("").expect_err("empty key should fail"),
            "Enter an API key before saving."
        );
    }

    #[test]
    fn rejects_whitespace_only_key() {
        assert_eq!(
            validate_api_key("   \t\n ").expect_err("blank key should fail"),
            "Enter an API key before saving."
        );
    }

    #[test]
    fn keeps_inner_whitespace_unchanged() {
        assert_eq!(
            validate_api_key("ab cd").expect("key should be valid"),
            "ab cd"
        );
    }
}
