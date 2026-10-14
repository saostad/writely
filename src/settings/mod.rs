#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "windows")]
mod windows;

const SERVICE: &str = "writely";
const USER: &str = "gemini-api-key";
pub(crate) const API_KEY_PAGE: &str = "https://aistudio.google.com/apikey";
pub(crate) const ACCESSIBILITY_SETTINGS_URL: &str =
    "x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility";

static PERMISSION_MESSAGE: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

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

pub fn save_api_key(key: &str) -> Result<(), String> {
    let trimmed = validate_api_key(key)?;
    let entry = keyring_entry()
        .map_err(|e| format!("Could not save the key to the system keychain: {e}"))?;
    entry
        .set_password(&trimmed)
        .map_err(|e| format!("Could not save the key to the system keychain: {e}"))
}

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
