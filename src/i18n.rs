use std::path::PathBuf;

/// Detect the system locale and set it for rust-i18n.
/// Priority: saved config → system detection → "en".
pub fn init_locale() -> &'static str {
    let lang = load_saved_locale()
        .unwrap_or_else(detect_language);
    rust_i18n::set_locale(lang);
    lang
}

/// Save the user's language preference to a config file next to the exe.
pub fn save_locale(lang: &str) {
    if let Some(path) = config_path() {
        let _ = std::fs::write(path, lang);
    }
}

fn load_saved_locale() -> Option<&'static str> {
    let path = config_path()?;
    let lang = std::fs::read_to_string(&path).ok()?;
    let lang = lang.trim();
    match lang {
        "zh" => Some("zh"),
        "en" => Some("en"),
        _ => None,
    }
}

fn config_path() -> Option<PathBuf> {
    std::env::current_exe()
        .ok()
        .map(|p| p.with_extension("lang"))
}

fn detect_language() -> &'static str {
    // On Windows, prefer system UI language over terminal env vars
    // (Git Bash / VS Code often sets LANG=en_US regardless of actual system language)
    #[cfg(target_os = "windows")]
    {
        if let Some(lang) = windows_ui_language() {
            if lang.starts_with("zh") {
                return "zh";
            }
            if !lang.is_empty() {
                return "en";
            }
        }
    }

    // Check LANG / LC_ALL / LC_CTYPE environment variables
    for var in &["LANG", "LC_ALL", "LC_CTYPE"] {
        if let Ok(val) = std::env::var(var) {
            let val = val.to_lowercase();
            if val.starts_with("zh") {
                return "zh";
            }
            if !val.is_empty() {
                return "en";
            }
        }
    }

    // Default to English for non-Chinese systems
    "en"
}

#[cfg(target_os = "windows")]
fn windows_ui_language() -> Option<String> {
    // Use GetUserDefaultUILanguage via a simple FFI call
    #[link(name = "kernel32")]
    extern "system" {
        fn GetUserDefaultUILanguage() -> u16;
    }

    let lang_id = unsafe { GetUserDefaultUILanguage() };
    // Primary language ID is the low 10 bits
    let primary_lang = lang_id & 0x3FF;
    // 0x04 = Chinese
    if primary_lang == 0x04 {
        return Some("zh".to_string());
    }
    Some("en".to_string())
}
