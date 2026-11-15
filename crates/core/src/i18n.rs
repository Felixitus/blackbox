//! German / English texts.
//!
//! The language is process-wide (set once by the app or the CLI; English until
//! then). Tests can
//! override it for their own thread with `with_lang`, so tests asserting
//! texts in different languages may run in parallel.
//!
//! Texts that end up in the database (journal entries of the Loadout Vault,
//! session notes, Windows setting values) are stored as language-neutral
//! codes and only rendered for display, so switching the language changes
//! old entries too and never creates spurious changes.

use std::cell::Cell;
use std::sync::atomic::{AtomicU8, Ordering};

use serde::{Deserialize, Serialize};

use crate::model::{Category, ChangeEvent, Fact, Timestamp};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Lang {
    De,
    En,
}

impl Lang {
    pub fn code(self) -> &'static str {
        match self {
            Lang::De => "de",
            Lang::En => "en",
        }
    }

    /// Locale for date and number formatting in the UI.
    pub fn locale(self) -> &'static str {
        match self {
            Lang::De => "de-DE",
            Lang::En => "en-US",
        }
    }
}

/// The language setting as stored in the config: `auto`, `de` or `en`.
pub fn resolve_setting(setting: &str) -> Lang {
    match setting.trim().to_ascii_lowercase().as_str() {
        "de" => Lang::De,
        "en" => Lang::En,
        _ => system_lang(),
    }
}

/// The Windows display language (German for any `de-*` locale, otherwise English).
pub fn system_lang() -> Lang {
    #[cfg(windows)]
    {
        use winreg::enums::{HKEY_CURRENT_USER, KEY_READ};
        let locale = winreg::RegKey::predef(HKEY_CURRENT_USER)
            .open_subkey_with_flags(r"Control Panel\International", KEY_READ)
            .and_then(|k| k.get_value::<String, _>("LocaleName"))
            .unwrap_or_default();
        if !locale.is_empty() {
            return lang_from_locale(&locale);
        }
    }
    let env = std::env::var("LANG").unwrap_or_default();
    lang_from_locale(&env)
}

pub fn lang_from_locale(locale: &str) -> Lang {
    if locale.to_ascii_lowercase().starts_with("de") { Lang::De } else { Lang::En }
}

static GLOBAL: AtomicU8 = AtomicU8::new(0);

thread_local! {
    static OVERRIDE: Cell<Option<Lang>> = const { Cell::new(None) };
}

pub fn set_lang(lang: Lang) {
    GLOBAL.store(if lang == Lang::De { 1 } else { 0 }, Ordering::Relaxed);
}

pub fn lang() -> Lang {
    OVERRIDE.with(|o| o.get()).unwrap_or_else(|| {
        if GLOBAL.load(Ordering::Relaxed) == 1 { Lang::De } else { Lang::En }
    })
}

/// Run `f` with `lang` for the current thread only.
pub fn with_lang<R>(lang: Lang, f: impl FnOnce() -> R) -> R {
    let previous = OVERRIDE.with(|o| o.replace(Some(lang)));
    let result = f();
    OVERRIDE.with(|o| o.set(previous));
    result
}

/// Pick the text for the current language.
pub fn t(de: &'static str, en: &'static str) -> &'static str {
    match lang() {
        Lang::De => de,
        Lang::En => en,
    }
}

/// `format!` in the current language: `tr!("Datei {name}", "File {name}")`.
#[macro_export]
macro_rules! tr {
    ($de:literal, $en:literal $(,)?) => {
        match $crate::i18n::lang() {
            $crate::i18n::Lang::De => format!($de),
            $crate::i18n::Lang::En => format!($en),
        }
    };
}

fn fmt_time(ts: Timestamp) -> String {
    let pattern = match lang() {
        Lang::De => "%d.%m.%Y %H:%M",
        Lang::En => "%Y-%m-%d %H:%M",
    };
    chrono::DateTime::from_timestamp(ts, 0)
        .map(|t| t.with_timezone(&chrono::Local).format(pattern).to_string())
        .unwrap_or_else(|| ts.to_string())
}

pub fn format_time(ts: Timestamp) -> String {
    fmt_time(ts)
}

// ---- language-neutral codes ------------------------------------------------------

/// Parse `kind;key=value;...`; a `msg=` parameter takes the rest of the line.
fn parse_code(code: &str) -> (&str, Vec<(&str, &str)>) {
    let mut kind_and_rest = code.splitn(2, ';');
    let kind = kind_and_rest.next().unwrap_or_default();
    let mut params = Vec::new();
    let mut rest = kind_and_rest.next().unwrap_or_default();
    while !rest.is_empty() {
        if let Some(msg) = rest.strip_prefix("msg=") {
            params.push(("msg", msg));
            break;
        }
        let (part, next) = rest.split_once(';').unwrap_or((rest, ""));
        if let Some((k, v)) = part.split_once('=') {
            params.push((k, v));
        }
        rest = next;
    }
    (kind, params)
}

fn param<'a>(params: &[(&str, &'a str)], key: &str) -> &'a str {
    params.iter().find(|(k, _)| *k == key).map(|(_, v)| *v).unwrap_or_default()
}

fn num(params: &[(&str, &str)], key: &str) -> i64 {
    param(params, key).parse().unwrap_or(0)
}

/// Journal value of a config file change detected by the Loadout Vault.
pub fn vault_changed_code(changed: usize, total: usize, reset: bool, game_updated: bool, binary: bool) -> String {
    format!(
        "vault:changed;n={changed};total={total};reset={};update={};binary={}",
        u8::from(reset),
        u8::from(game_updated),
        u8::from(binary)
    )
}

pub fn vault_restored_code(from: Timestamp) -> String {
    format!("vault:restored;at={from}")
}

pub fn vault_personal_code(keys: usize, from: Timestamp) -> String {
    format!("vault:personal;n={keys};at={from}")
}

pub fn vault_imported_code(keys: usize) -> String {
    format!("vault:imported;n={keys}")
}

/// Whether a journal value marks a possible settings reset (new codes and
/// texts written by earlier versions).
pub fn is_reset_value(value: &str) -> bool {
    match value.strip_prefix("vault:") {
        Some(code) => {
            let (kind, params) = parse_code(code);
            kind == "changed" && param(&params, "reset") == "1"
        }
        None => value.contains("möglicher Reset"),
    }
}

/// Text for a summary of changed settings.
pub fn changed_text(changed: i64, total: i64, reset: bool, game_updated: bool, binary: bool) -> String {
    let base = if binary {
        t("Datei geändert", "File changed").to_string()
    } else {
        tr!("{changed} von {total} Einträgen geändert", "{changed} of {total} settings changed")
    };
    match (reset, game_updated) {
        (true, true) => format!("{base} – {}", t("möglicher Reset nach Spiel-Update", "possible reset after a game update")),
        (true, false) => format!("{base} – {}", t("möglicher Reset", "possible reset")),
        (false, true) => format!("{base} ({})", t("nach Spiel-Update", "after a game update")),
        (false, false) => base,
    }
}

fn render_vault(code: &str) -> String {
    let (kind, p) = parse_code(code);
    match kind {
        "changed" => changed_text(
            num(&p, "n"),
            num(&p, "total"),
            param(&p, "reset") == "1",
            param(&p, "update") == "1",
            param(&p, "binary") == "1",
        ),
        "restored" => {
            let when = fmt_time(num(&p, "at"));
            tr!("Stand vom {when} wiederhergestellt", "Version from {when} restored")
        }
        "personal" => {
            let (n, when) = (num(&p, "n"), fmt_time(num(&p, "at")));
            tr!("{n} persönliche Einstellung(en) vom {when} übernommen", "{n} personal setting(s) from {when} applied")
        }
        "imported" => {
            let n = num(&p, "n");
            tr!("{n} persönliche Einstellung(en) aus Profil übernommen", "{n} personal setting(s) applied from profile")
        }
        _ => code.to_string(),
    }
}

/// Session notes are stored as codes; texts of earlier versions pass through.
pub fn note_no_presentmon() -> String {
    "note:no_presentmon".into()
}
pub fn note_presentmon_exited() -> String {
    "note:presentmon_exited".into()
}
pub fn note_too_few_frames(have: usize, need: usize) -> String {
    format!("note:too_few;n={have};min={need}")
}
pub fn note_unreadable(msg: &str) -> String {
    format!("note:unreadable;msg={msg}")
}
pub fn note_capture_failed(msg: &str) -> String {
    format!("note:capture;msg={msg}")
}
pub fn note_imported() -> String {
    "note:imported".into()
}
pub fn note_interrupted() -> String {
    "note:interrupted".into()
}

pub fn capture_note(raw: &str) -> String {
    let Some(code) = raw.strip_prefix("note:") else { return raw.to_string() };
    let (kind, p) = parse_code(code);
    match kind {
        "no_presentmon" => t("Kein PresentMon gefunden", "PresentMon not found").into(),
        "presentmon_exited" => t(
            "PresentMon wurde vorzeitig beendet – fehlende Rechte (Einstellungen › Frame-Messung dauerhaft erlauben)",
            "PresentMon stopped early – missing rights (Settings › Allow frame capture permanently)",
        )
        .into(),
        "too_few" => {
            let (n, min) = (num(&p, "n"), num(&p, "min"));
            tr!("Zu wenige Frames für eine Auswertung ({n} von mindestens {min})", "Too few frames to evaluate ({n} of at least {min})")
        }
        "unreadable" => {
            let msg = param(&p, "msg");
            tr!("Aufzeichnung nicht lesbar: {msg}", "Capture unreadable: {msg}")
        }
        "capture" => {
            let msg = param(&p, "msg");
            tr!("Frame-Messung nicht möglich: {msg}", "Frame capture unavailable: {msg}")
        }
        "imported" => t("Importiert", "Imported").into(),
        "interrupted" => t("Aufzeichnung wurde unterbrochen", "Recording was interrupted").into(),
        _ => raw.to_string(),
    }
}

// ---- system facts -----------------------------------------------------------------

/// Display name of a fact whose label is fixed by Blackbox.
pub fn fact_label(category: Category, key: &str, stored: &str) -> String {
    let fixed = match (category, key) {
        (Category::OsBuild, "windows") => Some(("Windows", "Windows")),
        (Category::Bios, "bios") => Some(("BIOS/UEFI-Firmware", "BIOS/UEFI firmware")),
        (Category::PowerPlan, "active") => Some(("Aktiver Energieplan", "Active power plan")),
        (Category::SystemSetting, "game_mode") => Some(("Spielmodus", "Game Mode")),
        (Category::SystemSetting, "game_dvr") => Some(("Hintergrundaufzeichnung (Game DVR)", "Background recording (Game DVR)")),
        (Category::SystemSetting, "hags") => Some(("Hardwarebeschleunigte GPU-Planung", "Hardware-accelerated GPU scheduling")),
        (Category::SystemSetting, "memory_integrity") => Some(("Speicher-Integrität (Kernisolierung)", "Memory integrity (core isolation)")),
        (Category::SystemSetting, "vbs") => Some(("Virtualisierungsbasierte Sicherheit (VBS)", "Virtualization-based security (VBS)")),
        _ => None,
    };
    match fixed {
        Some((de, en)) => t(de, en).to_string(),
        None => stored.to_string(),
    }
}

/// Display text of a fact value. Codes of current and earlier versions
/// (which stored German words) are both understood.
pub fn fact_value(category: Category, value: &str) -> String {
    match category {
        Category::SystemSetting => match value {
            "on" | "an" => t("an", "on").into(),
            "off" | "aus" => t("aus", "off").into(),
            "default" | "Standard" => t("Standard", "default").into(),
            v => match v.strip_prefix("unknown:") {
                Some(n) => tr!("unbekannt ({n})", "unknown ({n})"),
                None => v.to_string(),
            },
        },
        Category::WindowsUpdate if value == "installed" || value == "installiert" => t("installiert", "installed").into(),
        Category::GameBuild => match value.strip_prefix("files ").or_else(|| value.strip_prefix("Dateistand ")) {
            Some(date) => tr!("Dateistand {date}", "files from {date}"),
            None => value.to_string(),
        },
        _ => value.to_string(),
    }
}

pub fn display_fact(f: &Fact) -> Fact {
    Fact {
        category: f.category,
        key: f.key.clone(),
        label: fact_label(f.category, &f.key, &f.label),
        value: fact_value(f.category, &f.value),
    }
}

/// A journal entry as it is shown: labels and values in the current language.
pub fn display_change(c: &ChangeEvent) -> ChangeEvent {
    let value = |v: &Option<String>| {
        v.as_ref().map(|v| match v.strip_prefix("vault:") {
            Some(code) => render_vault(code),
            None => fact_value(c.category, v),
        })
    };
    ChangeEvent {
        label: fact_label(c.category, &c.key, &c.label),
        old_value: value(&c.old_value),
        new_value: value(&c.new_value),
        ..c.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::ChangeKind;

    #[test]
    fn locale_detection() {
        assert_eq!(lang_from_locale("de-DE"), Lang::De);
        assert_eq!(lang_from_locale("de-AT"), Lang::De);
        assert_eq!(lang_from_locale("en-US"), Lang::En);
        assert_eq!(lang_from_locale("fr-FR"), Lang::En);
        assert_eq!(resolve_setting("DE"), Lang::De);
        assert_eq!(resolve_setting("en"), Lang::En);
    }

    #[test]
    fn thread_override_does_not_leak() {
        let outer = lang();
        let inner = with_lang(Lang::En, || {
            assert_eq!(t("Ja", "Yes"), "Yes");
            let n = 3;
            tr!("{n} Dateien", "{n} files")
        });
        assert_eq!(inner, "3 files");
        assert_eq!(lang(), outer);
        let other = std::thread::spawn(|| with_lang(Lang::De, || t("Ja", "Yes"))).join().unwrap();
        assert_eq!(other, "Ja");
    }

    #[test]
    fn vault_codes_render_in_both_languages() {
        let code = vault_changed_code(9, 20, true, true, false);
        assert!(is_reset_value(&code));
        assert!(!is_reset_value(&vault_changed_code(1, 20, false, false, false)));
        assert!(is_reset_value("9 von 20 Einträgen geändert – möglicher Reset"), "texts of earlier versions");
        let c = ChangeEvent {
            id: 1,
            detected_at: 0,
            category: Category::GameConfig,
            key: "steam:1".into(),
            label: "Game: a.ini".into(),
            kind: ChangeKind::Modified,
            old_value: None,
            new_value: Some(code),
        };
        assert_eq!(
            with_lang(Lang::De, || display_change(&c)).new_value.as_deref(),
            Some("9 von 20 Einträgen geändert – möglicher Reset nach Spiel-Update")
        );
        assert_eq!(
            with_lang(Lang::En, || display_change(&c)).new_value.as_deref(),
            Some("9 of 20 settings changed – possible reset after a game update")
        );
        let imported = ChangeEvent { new_value: Some(vault_imported_code(3)), ..c.clone() };
        assert_eq!(with_lang(Lang::En, || display_change(&imported)).new_value.as_deref(), Some("3 personal setting(s) applied from profile"));
        let legacy = ChangeEvent { new_value: Some("alter deutscher Text".into()), ..c };
        assert_eq!(with_lang(Lang::En, || display_change(&legacy)).new_value.as_deref(), Some("alter deutscher Text"));
    }

    #[test]
    fn notes_and_facts() {
        assert_eq!(with_lang(Lang::En, || capture_note(&note_too_few_frames(12, 3000))), "Too few frames to evaluate (12 of at least 3000)");
        assert_eq!(with_lang(Lang::De, || capture_note(&note_unreadable("a; b"))), "Aufzeichnung nicht lesbar: a; b");
        assert_eq!(with_lang(Lang::En, || capture_note("Kein PresentMon konfiguriert")), "Kein PresentMon konfiguriert");
        assert_eq!(with_lang(Lang::En, || fact_value(Category::SystemSetting, "an")), "on");
        assert_eq!(with_lang(Lang::En, || fact_value(Category::SystemSetting, "unknown:3")), "unknown (3)");
        assert_eq!(with_lang(Lang::De, || fact_value(Category::SystemSetting, "default")), "Standard");
        assert_eq!(with_lang(Lang::En, || fact_value(Category::GameBuild, "files 2026-06-28 00:08")), "files from 2026-06-28 00:08");
        assert_eq!(with_lang(Lang::En, || fact_label(Category::SystemSetting, "hags", "Hardwarebeschleunigte GPU-Planung")), "Hardware-accelerated GPU scheduling");
        assert_eq!(with_lang(Lang::En, || fact_label(Category::GpuDriver, "RX", "AMD Radeon RX 7900 XTX")), "AMD Radeon RX 7900 XTX");
    }
}
