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
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
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
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
static HOTKEY_MESSAGE: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);
static AUTOSTART_MESSAGE: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

pub fn set_autostart_message(message: Option<String>) {
    match AUTOSTART_MESSAGE.lock() {
        Ok(mut guard) => *guard = message,
        Err(poisoned) => *poisoned.into_inner() = message,
    }
}

pub fn autostart_message() -> Option<String> {
    match AUTOSTART_MESSAGE.lock() {
        Ok(guard) => guard.clone(),
        Err(poisoned) => poisoned.into_inner().clone(),
    }
}

#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub fn set_hotkey_message(message: Option<String>) {
    match HOTKEY_MESSAGE.lock() {
        Ok(mut guard) => *guard = message,
        Err(poisoned) => *poisoned.into_inner() = message,
    }
}

#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
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

#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
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

/// Slot in a `[String; 3]` key-draft store, in provider order (Gemini,
/// Together, Ollama). Ollama needs no key, so it has no slot: a typed key is
/// never filed or restored for it.
pub(crate) fn draft_slot(provider: crate::config::Provider) -> Option<usize> {
    match provider {
        crate::config::Provider::Gemini => Some(0),
        crate::config::Provider::Together => Some(1),
        crate::config::Provider::Ollama => None,
    }
}

/// Files the key box's current text under the shown provider. Empty (or
/// whitespace-only) text clears that provider's draft. Ollama is never filed.
pub(crate) fn file_key_draft(drafts: &mut [String; 3], shown: crate::config::Provider, text: &str) {
    if let Some(slot) = draft_slot(shown) {
        if text.trim().is_empty() {
            drafts[slot].clear();
        } else {
            drafts[slot] = text.to_string();
        }
    }
}

/// The draft to show in the key box for `provider` (empty when none).
/// Always empty for Ollama.
pub(crate) fn restore_key_draft(drafts: &[String; 3], provider: crate::config::Provider) -> &str {
    match draft_slot(provider) {
        Some(slot) => &drafts[slot],
        None => "",
    }
}

/// Every non-empty draft in provider order, each with its own provider, for
/// Save. Keys are trimmed; Ollama never has one.
pub(crate) fn pending_key_drafts(drafts: &[String; 3]) -> Vec<(crate::config::Provider, String)> {
    [
        crate::config::Provider::Gemini,
        crate::config::Provider::Together,
    ]
    .into_iter()
    .filter_map(|provider| {
        let slot = draft_slot(provider)?;
        let key = drafts[slot].trim();
        if key.is_empty() {
            None
        } else {
            Some((provider, key.to_string()))
        }
    })
    .collect()
}

/// Drops one provider's draft (Remove key, or after its key is saved).
pub(crate) fn clear_key_draft(drafts: &mut [String; 3], provider: crate::config::Provider) {
    if let Some(slot) = draft_slot(provider) {
        drafts[slot].clear();
    }
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
    use super::{
        clear_key_draft, file_key_draft, pending_key_drafts, restore_key_draft, validate_api_key,
    };
    use crate::config::Provider;

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

    #[test]
    fn draft_round_trip_gemini_together_gemini() {
        let mut drafts = [String::new(), String::new(), String::new()];
        // Type a Gemini key, then switch to Together: the Together box is empty.
        file_key_draft(&mut drafts, Provider::Gemini, "gemini-key");
        assert_eq!(restore_key_draft(&drafts, Provider::Together), "");
        // Type a Together key, then switch back: the Gemini text is restored.
        file_key_draft(&mut drafts, Provider::Together, "together-key");
        assert_eq!(restore_key_draft(&drafts, Provider::Gemini), "gemini-key");
        // Each draft saves under its own provider.
        assert_eq!(
            pending_key_drafts(&drafts),
            vec![
                (Provider::Gemini, "gemini-key".to_string()),
                (Provider::Together, "together-key".to_string()),
            ]
        );
    }

    #[test]
    fn ollama_is_never_filed() {
        let mut drafts = [String::new(), String::new(), String::new()];
        file_key_draft(&mut drafts, Provider::Gemini, "gemini-key");
        file_key_draft(&mut drafts, Provider::Ollama, "should-not-store");
        assert_eq!(restore_key_draft(&drafts, Provider::Ollama), "");
        assert!(drafts.iter().all(|draft| draft != "should-not-store"));
        // The Ollama filing left the other drafts alone.
        assert_eq!(restore_key_draft(&drafts, Provider::Gemini), "gemini-key");
        assert_eq!(pending_key_drafts(&drafts).len(), 1);
    }

    #[test]
    fn empty_text_clears_the_draft() {
        let mut drafts = [String::new(), String::new(), String::new()];
        file_key_draft(&mut drafts, Provider::Gemini, "gemini-key");
        file_key_draft(&mut drafts, Provider::Gemini, "");
        assert_eq!(restore_key_draft(&drafts, Provider::Gemini), "");
        file_key_draft(&mut drafts, Provider::Together, "together-key");
        file_key_draft(&mut drafts, Provider::Together, "   \t\n ");
        assert_eq!(restore_key_draft(&drafts, Provider::Together), "");
        assert!(pending_key_drafts(&drafts).is_empty());
    }

    #[test]
    fn clearing_drops_one_draft() {
        let mut drafts = [String::new(), String::new(), String::new()];
        file_key_draft(&mut drafts, Provider::Gemini, "gemini-key");
        file_key_draft(&mut drafts, Provider::Together, "together-key");
        clear_key_draft(&mut drafts, Provider::Gemini);
        assert_eq!(restore_key_draft(&drafts, Provider::Gemini), "");
        assert_eq!(
            restore_key_draft(&drafts, Provider::Together),
            "together-key"
        );
    }
}
