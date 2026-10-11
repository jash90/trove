//! The language of the strings the shell itself shows: the menu bar item, the
//! window titles and the alert a failed launch puts up.
//!
//! English is the default and the fallback; Polish is used when the user's
//! most preferred language that Trove can tell apart is Polish — unless the
//! `language` setting names one outright, which outranks the system. The
//! interface reads the same answer through `get_locale`, so the menu bar and
//! the windows never disagree.

use std::sync::OnceLock;
use std::sync::atomic::{AtomicU8, Ordering};

/// Sent to every window, with the new code, when a saved setting changes the
/// language.
pub const LOCALE_CHANGED_EVENT: &str = "locale-changed";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Locale {
    En,
    Pl,
}

impl Locale {
    pub fn code(self) -> &'static str {
        match self {
            Self::En => "en",
            Self::Pl => "pl",
        }
    }
}

/// What the `language` setting asks for: the system's answer, or one
/// language regardless of it.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Preference {
    #[default]
    System,
    En,
    Pl,
}

impl Preference {
    fn forced(self) -> Option<Locale> {
        match self {
            Self::System => None,
            Self::En => Some(Locale::En),
            Self::Pl => Some(Locale::Pl),
        }
    }
}

/// Picks the locale from preferred-language tags, most preferred first.
///
/// The first tag that is either English or Polish decides; anything else is
/// skipped, and a list with neither falls back to English.
pub fn detect<S: AsRef<str>>(languages: &[S]) -> Locale {
    for language in languages {
        let tag = language.as_ref().trim().to_ascii_lowercase();
        let primary = tag.split(['-', '_', '.', '@']).next().unwrap_or_default();
        match primary {
            "pl" => return Locale::Pl,
            "en" => return Locale::En,
            _ => {}
        }
    }
    Locale::En
}

fn system_languages() -> Vec<String> {
    let mut languages = platform_macos::locale::preferred_languages();
    // The POSIX variables, in the order the C library consults them. On macOS
    // they are a fallback; elsewhere they are the whole answer.
    for variable in ["LC_ALL", "LC_MESSAGES", "LANG"] {
        if let Ok(value) = std::env::var(variable)
            && !value.is_empty()
            && value != "C"
            && value != "POSIX"
        {
            languages.push(value);
        }
    }
    languages
}

/// The system's answer, read once: the preferred languages do not change
/// under a running process in any way worth chasing.
fn system() -> Locale {
    static SYSTEM: OnceLock<Locale> = OnceLock::new();
    *SYSTEM.get_or_init(|| detect(&system_languages()))
}

/// The `language` setting, as last applied: 0 follows the system, 1 is
/// English, 2 is Polish. An atomic rather than a lock because it is read on
/// every string the shell shows and written only on launch and on save.
static PREFERENCE: AtomicU8 = AtomicU8::new(0);

/// Applies the `language` setting. Returns whether the locale changed, which
/// is what decides whether the menu bar and the windows need retexting.
pub fn set_preference(preference: Preference) -> bool {
    let before = current();
    let encoded = match preference {
        Preference::System => 0,
        Preference::En => 1,
        Preference::Pl => 2,
    };
    PREFERENCE.store(encoded, Ordering::Relaxed);
    current() != before
}

fn preference() -> Preference {
    match PREFERENCE.load(Ordering::Relaxed) {
        1 => Preference::En,
        2 => Preference::Pl,
        _ => Preference::System,
    }
}

/// The locale in force: the setting when it names a language, the system's
/// answer otherwise.
pub fn resolve(preference: Preference, system: Locale) -> Locale {
    preference.forced().unwrap_or(system)
}

/// The locale in force now.
pub fn current() -> Locale {
    resolve(preference(), system())
}

/// Every string the shell shows.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Text {
    TrayShow,
    TraySettings,
    TrayPause,
    TrayResume,
    TrayCheckForUpdates,
    TrayQuit,
    SettingsWindowTitle,
    ChatWindowTitle,
    StartupSchemaNewerTitle,
    StartupSchemaNewerDetail,
    StartupPermissionsTitle,
    StartupPermissionsDetail,
    StartupLockedTitle,
    StartupLockedDetail,
    StartupUnavailableTitle,
    StartupUnavailableDetail,
    AlertShowDataFolder,
    AlertQuit,
}

impl Text {
    pub const ALL: [Text; 18] = [
        Text::TrayShow,
        Text::TraySettings,
        Text::TrayPause,
        Text::TrayResume,
        Text::TrayCheckForUpdates,
        Text::TrayQuit,
        Text::SettingsWindowTitle,
        Text::ChatWindowTitle,
        Text::StartupSchemaNewerTitle,
        Text::StartupSchemaNewerDetail,
        Text::StartupPermissionsTitle,
        Text::StartupPermissionsDetail,
        Text::StartupLockedTitle,
        Text::StartupLockedDetail,
        Text::StartupUnavailableTitle,
        Text::StartupUnavailableDetail,
        Text::AlertShowDataFolder,
        Text::AlertQuit,
    ];
}

/// The string for `text` in `locale`. The match is exhaustive in both
/// languages, so a string added to one and not the other does not compile.
pub fn text(locale: Locale, text: Text) -> &'static str {
    match locale {
        Locale::En => english(text),
        Locale::Pl => polish(text),
    }
}

/// The string for `text` in this run's locale.
pub fn tr(text_id: Text) -> &'static str {
    text(current(), text_id)
}

fn english(text: Text) -> &'static str {
    match text {
        Text::TrayShow => "Show history",
        Text::TraySettings => "Settings…",
        Text::TrayPause => "Pause capture",
        Text::TrayResume => "Resume capture",
        Text::TrayCheckForUpdates => "Check for updates…",
        Text::TrayQuit => "Quit",
        Text::SettingsWindowTitle => "Settings — Trove",
        Text::ChatWindowTitle => "Chat — Trove",
        Text::StartupSchemaNewerTitle => "Trove cannot open your history",
        Text::StartupSchemaNewerDetail => {
            "It was saved by a newer version of Trove. Install the latest \
             version to open it; this one has left it untouched."
        }
        Text::StartupPermissionsTitle => "Trove cannot use its data folder",
        Text::StartupPermissionsDetail => {
            "The folder must belong to you and be readable only by you. \
             Check its owner and permissions, then open Trove again."
        }
        Text::StartupLockedTitle => "Trove's history is in use",
        Text::StartupLockedDetail => {
            "Another copy of Trove, or another program, has the history \
             open. Quit it and open Trove again."
        }
        Text::StartupUnavailableTitle => "Trove cannot open its storage",
        Text::StartupUnavailableDetail => {
            "The history database could not be opened. The details are in \
             ~/Library/Logs/Trove/trove.log."
        }
        Text::AlertShowDataFolder => "Show data folder",
        Text::AlertQuit => "Quit",
    }
}

fn polish(text: Text) -> &'static str {
    match text {
        Text::TrayShow => "Pokaż historię",
        Text::TraySettings => "Ustawienia…",
        Text::TrayPause => "Wstrzymaj przechwytywanie",
        Text::TrayResume => "Wznów przechwytywanie",
        Text::TrayCheckForUpdates => "Sprawdź aktualizacje…",
        Text::TrayQuit => "Zakończ",
        Text::SettingsWindowTitle => "Ustawienia — Trove",
        Text::ChatWindowTitle => "Czat — Trove",
        Text::StartupSchemaNewerTitle => "Trove nie może otworzyć Twojej historii",
        Text::StartupSchemaNewerDetail => {
            "Została zapisana przez nowszą wersję Trove. Zainstaluj najnowszą \
             wersję, aby ją otworzyć; ta wersja zostawiła ją nietkniętą."
        }
        Text::StartupPermissionsTitle => "Trove nie może użyć swojego folderu danych",
        Text::StartupPermissionsDetail => {
            "Folder musi należeć do Ciebie i być czytelny tylko dla Ciebie. \
             Sprawdź jego właściciela i uprawnienia, a potem otwórz Trove ponownie."
        }
        Text::StartupLockedTitle => "Historia Trove jest w użyciu",
        Text::StartupLockedDetail => {
            "Inna kopia Trove albo inny program ma otwartą historię. \
             Zamknij go i otwórz Trove ponownie."
        }
        Text::StartupUnavailableTitle => "Trove nie może otworzyć swojej pamięci",
        Text::StartupUnavailableDetail => {
            "Nie udało się otworzyć bazy historii. Szczegóły są w \
             ~/Library/Logs/Trove/trove.log."
        }
        Text::AlertShowDataFolder => "Pokaż folder danych",
        Text::AlertQuit => "Zakończ",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn polish_is_chosen_only_when_it_comes_first_among_the_known_languages() {
        assert_eq!(detect(&["pl-PL", "en-US"]), Locale::Pl);
        assert_eq!(detect(&["pl"]), Locale::Pl);
        assert_eq!(detect(&["pl_PL.UTF-8"]), Locale::Pl);
        assert_eq!(detect(&["en-GB", "pl-PL"]), Locale::En);
        // A language Trove does not speak is skipped, not treated as English.
        assert_eq!(detect(&["de-DE", "pl-PL"]), Locale::Pl);
        assert_eq!(detect(&["de-DE"]), Locale::En);
        assert_eq!(detect::<&str>(&[]), Locale::En);
        // `plt` is not Polish.
        assert_eq!(detect(&["plt"]), Locale::En);
    }

    #[test]
    fn both_languages_have_every_string() {
        for text_id in Text::ALL {
            assert!(!text(Locale::En, text_id).is_empty(), "{text_id:?} en");
            assert!(!text(Locale::Pl, text_id).is_empty(), "{text_id:?} pl");
        }
    }

    #[test]
    fn a_named_language_outranks_the_system_and_system_defers_to_it() {
        assert_eq!(resolve(Preference::Pl, Locale::En), Locale::Pl);
        assert_eq!(resolve(Preference::En, Locale::Pl), Locale::En);
        assert_eq!(resolve(Preference::System, Locale::Pl), Locale::Pl);
        assert_eq!(resolve(Preference::System, Locale::En), Locale::En);
    }

    #[test]
    fn the_preference_reads_and_writes_the_interface_codes() {
        assert_eq!(
            serde_json::from_str::<Preference>("\"pl\"").unwrap(),
            Preference::Pl
        );
        assert_eq!(
            serde_json::to_string(&Preference::System).unwrap(),
            "\"system\""
        );
        assert!(serde_json::from_str::<Preference>("\"de\"").is_err());
    }

    #[test]
    fn the_codes_match_the_interface_locales() {
        assert_eq!(Locale::En.code(), "en");
        assert_eq!(Locale::Pl.code(), "pl");
    }
}
