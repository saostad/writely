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
        crate::config::Provider::Local => None,
    }
}

fn keychain_user_for(provider: crate::config::Provider) -> Option<&'static str> {
    match provider {
        crate::config::Provider::Gemini => Some(USER),
        crate::config::Provider::Together => Some(TOGETHER_USER),
        crate::config::Provider::Ollama => None,
        crate::config::Provider::Local => None,
    }
}

static PERMISSION_MESSAGE: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
static HOTKEY_MESSAGE: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);
static AUTOSTART_MESSAGE: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);
static PROVIDER_MESSAGE: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

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

pub fn set_provider_message(message: Option<String>) {
    match PROVIDER_MESSAGE.lock() {
        Ok(mut guard) => *guard = message,
        Err(poisoned) => *poisoned.into_inner() = message,
    }
}

pub fn provider_message() -> Option<String> {
    match PROVIDER_MESSAGE.lock() {
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
        crate::config::Provider::Local => None,
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

pub(crate) fn local_model_choices() -> Vec<(crate::config::LocalModelId, String)> {
    [
        crate::config::LocalModelId::Qwen35_0_8B,
        crate::config::LocalModelId::Qwen35_2B,
    ]
    .into_iter()
    .map(|id| {
        let model = crate::local::catalog::model_file(id);
        (
            model.id,
            format!(
                "{} ({} download)",
                model.label,
                crate::local::download::human_size(model.size)
            ),
        )
    })
    .collect()
}

pub(crate) struct LocalView {
    pub status: String,
    pub is_error: bool,
    pub show_download: bool,
    pub show_cancel: bool,
    pub show_delete: bool,
    pub fields_enabled: bool,
}

pub(crate) fn local_view(
    phase: &crate::local::install::InstallPhase,
    selected_ready: bool,
    _selected_label: &str,
) -> LocalView {
    use crate::local::install::InstallPhase;
    match phase {
        InstallPhase::Idle => {
            if selected_ready {
                LocalView {
                    status: "Ready. The model runs on this computer; your text doesn't leave it."
                        .to_string(),
                    is_error: false,
                    show_download: false,
                    show_cancel: false,
                    show_delete: true,
                    fields_enabled: true,
                }
            } else {
                LocalView {
                    status: "Not downloaded yet. Click Download to get it (nothing is downloaded until you do)."
                        .to_string(),
                    is_error: false,
                    show_download: true,
                    show_cancel: false,
                    show_delete: false,
                    fields_enabled: true,
                }
            }
        }
        InstallPhase::Downloading { what, done, total } => {
            let pct = if *total == 0 {
                0
            } else {
                (*done as u128 * 100 / *total as u128) as u64
            };
            LocalView {
                status: format!(
                    "Downloading {what}: {pct}% ({} of {})",
                    crate::local::download::human_size(*done),
                    crate::local::download::human_size(*total)
                ),
                is_error: false,
                show_download: false,
                show_cancel: true,
                show_delete: false,
                fields_enabled: false,
            }
        }
        InstallPhase::Verifying { what } => LocalView {
            status: format!("Checking the download of {what}…"),
            is_error: false,
            show_download: false,
            show_cancel: true,
            show_delete: false,
            fields_enabled: false,
        },
        InstallPhase::Unpacking => LocalView {
            status: "Unpacking the llama.cpp helper…".to_string(),
            is_error: false,
            show_download: false,
            show_cancel: false,
            show_delete: false,
            fields_enabled: false,
        },
        InstallPhase::Failed(msg) => {
            if msg == "Download cancelled." {
                LocalView {
                    status: "Download cancelled.".to_string(),
                    is_error: false,
                    show_download: true,
                    show_cancel: false,
                    show_delete: false,
                    fields_enabled: true,
                }
            } else {
                LocalView {
                    status: format!("Download failed: {msg}"),
                    is_error: true,
                    show_download: true,
                    show_cancel: false,
                    show_delete: false,
                    fields_enabled: true,
                }
            }
        }
    }
}

pub(crate) struct LocalFields {
    pub idle_minutes: String,
    pub threads: String,
    pub max_reply: String,
    pub context: String,
    pub startup_secs: String,
    pub reply_secs: String,
}

fn parse_local_field(text: &str, name: &str) -> Result<u32, String> {
    text.trim()
        .parse::<u32>()
        .map_err(|_| format!("{name} must be a whole number."))
}

pub(crate) fn apply_local_fields(
    settings: &mut crate::config::AppSettings,
    model: crate::config::LocalModelId,
    warm_up: bool,
    fields: &LocalFields,
) -> Result<(), String> {
    let idle_minutes = parse_local_field(&fields.idle_minutes, "Free memory after")?;
    let threads = parse_local_field(&fields.threads, "Processor threads")?;
    let max_reply = parse_local_field(&fields.max_reply, "Longest reply")?;
    let context = parse_local_field(&fields.context, "Context size")?;
    let startup_secs = parse_local_field(&fields.startup_secs, "Startup time limit")?;
    let reply_secs = parse_local_field(&fields.reply_secs, "Reply time limit")?;
    let mut copy = settings.clone();
    copy.local_model = model;
    copy.local_warm_up = warm_up;
    copy.local_idle_minutes = idle_minutes;
    copy.local_threads = threads;
    copy.local_max_reply_tokens = max_reply;
    copy.local_context_tokens = context;
    copy.local_startup_timeout_secs = startup_secs;
    copy.local_reply_timeout_secs = reply_secs;
    crate::config::validate_local_settings(&copy)?;
    *settings = copy;
    Ok(())
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

static PROXY: std::sync::OnceLock<tao::event_loop::EventLoopProxy<crate::UserEvent>> =
    std::sync::OnceLock::new();

/// Stores the event-loop proxy so Save and Remove key can send
/// `UserEvent::SettingsChanged`. Call once at startup.
pub fn init(proxy: tao::event_loop::EventLoopProxy<crate::UserEvent>) {
    if PROXY.set(proxy).is_err() {
        // Keep the first proxy installed for this process.
    }
}

/// Tells the app the settings changed (the tray menu refreshes itself).
pub(crate) fn notify_changed() {
    if let Some(proxy) = PROXY.get() {
        if let Err(error) = proxy.send_event(crate::UserEvent::SettingsChanged) {
            eprintln!("Writely: could not deliver the settings change: {error}");
        }
    }
}

/// Follows a tray switch in an open Settings window. Returns straight away
/// when Settings is not open (an `Option` check, nothing else).
pub fn provider_switched(provider: crate::config::Provider) {
    #[cfg(target_os = "macos")]
    macos::provider_switched(provider);
    #[cfg(target_os = "windows")]
    windows::provider_switched(provider);
    #[cfg(target_os = "linux")]
    linux::provider_switched(provider);
    #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
    let _ = provider;
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
        apply_local_fields, clear_key_draft, file_key_draft, local_model_choices, local_view,
        pending_key_drafts, restore_key_draft, validate_api_key, LocalFields,
    };
    use crate::config::{AppSettings, LocalModelId, Provider};
    use crate::local::install::InstallPhase;

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
    fn local_model_choices_have_exact_texts() {
        assert_eq!(
            local_model_choices(),
            vec![
                (
                    LocalModelId::Qwen35_0_8B,
                    "Qwen3.5 0.8B — fast (533 MB download)".to_string()
                ),
                (
                    LocalModelId::Qwen35_2B,
                    "Qwen3.5 2B — better quality, slower (1.3 GB download)".to_string()
                ),
            ]
        );
    }

    #[test]
    fn local_view_for_every_phase() {
        let ready = local_view(&InstallPhase::Idle, true, "label");
        assert_eq!(
            ready.status,
            "Ready. The model runs on this computer; your text doesn't leave it."
        );
        assert!(!ready.is_error && !ready.show_download && !ready.show_cancel);
        assert!(ready.show_delete && ready.fields_enabled);

        let missing = local_view(&InstallPhase::Idle, false, "label");
        assert_eq!(
            missing.status,
            "Not downloaded yet. Click Download to get it (nothing is downloaded until you do)."
        );
        assert!(missing.show_download && !missing.show_delete && missing.fields_enabled);

        let downloading = local_view(
            &InstallPhase::Downloading {
                what: "the thing".to_string(),
                done: 1_500_000,
                total: 4_000_000,
            },
            false,
            "label",
        );
        assert_eq!(
            downloading.status,
            "Downloading the thing: 37% (2 MB of 4 MB)"
        );
        assert!(downloading.show_cancel && !downloading.fields_enabled);

        let zero = local_view(
            &InstallPhase::Downloading {
                what: "the thing".to_string(),
                done: 0,
                total: 0,
            },
            false,
            "label",
        );
        assert_eq!(zero.status, "Downloading the thing: 0% (0 B of 0 B)");

        let verifying = local_view(
            &InstallPhase::Verifying {
                what: "the thing".to_string(),
            },
            false,
            "label",
        );
        assert_eq!(verifying.status, "Checking the download of the thing…");
        assert!(verifying.show_cancel && !verifying.fields_enabled);

        let unpacking = local_view(&InstallPhase::Unpacking, false, "label");
        assert_eq!(unpacking.status, "Unpacking the llama.cpp helper…");
        assert!(!unpacking.show_cancel && !unpacking.fields_enabled);

        let failed = local_view(
            &InstallPhase::Failed("disk exploded".to_string()),
            false,
            "label",
        );
        assert_eq!(failed.status, "Download failed: disk exploded");
        assert!(failed.is_error && failed.show_download && failed.fields_enabled);

        let cancelled = local_view(
            &InstallPhase::Failed("Download cancelled.".to_string()),
            false,
            "label",
        );
        assert_eq!(cancelled.status, "Download cancelled.");
        assert!(!cancelled.is_error && cancelled.show_download);
    }

    fn valid_local_fields() -> LocalFields {
        LocalFields {
            idle_minutes: " 5 ".to_string(),
            threads: "8".to_string(),
            max_reply: "512".to_string(),
            context: "8192".to_string(),
            startup_secs: "90".to_string(),
            reply_secs: "300".to_string(),
        }
    }

    #[test]
    fn apply_local_fields_applies_valid_set() {
        let mut settings = AppSettings::default();
        apply_local_fields(
            &mut settings,
            LocalModelId::Qwen35_2B,
            false,
            &valid_local_fields(),
        )
        .expect("apply");
        assert_eq!(settings.local_model, LocalModelId::Qwen35_2B);
        assert!(!settings.local_warm_up);
        assert_eq!(settings.local_idle_minutes, 5);
        assert_eq!(settings.local_threads, 8);
        assert_eq!(settings.local_max_reply_tokens, 512);
        assert_eq!(settings.local_context_tokens, 8192);
        assert_eq!(settings.local_startup_timeout_secs, 90);
        assert_eq!(settings.local_reply_timeout_secs, 300);
    }

    #[test]
    fn apply_local_fields_rejects_non_number_unchanged() {
        let mut settings = AppSettings::default();
        let before = settings.clone();
        let mut fields = valid_local_fields();
        fields.threads = "many".to_string();
        assert_eq!(
            apply_local_fields(&mut settings, LocalModelId::Qwen35_2B, false, &fields)
                .expect_err("non-number"),
            "Processor threads must be a whole number."
        );
        assert_eq!(settings, before);
    }

    #[test]
    fn apply_local_fields_rejects_out_of_range_unchanged() {
        let mut settings = AppSettings::default();
        let before = settings.clone();
        let mut fields = valid_local_fields();
        fields.idle_minutes = "1441".to_string();
        assert_eq!(
            apply_local_fields(&mut settings, LocalModelId::Qwen35_2B, false, &fields)
                .expect_err("out of range"),
            "Free memory after must be between 0 and 1440 minutes (0 means never)."
        );
        assert_eq!(settings, before);
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
