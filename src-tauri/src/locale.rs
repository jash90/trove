//! The language of the strings the shell itself shows: the menu bar item, the
//! window titles and the alert a failed launch puts up.
//!
//! English is the default and the fallback; Polish is used when the user's
//! most preferred language that Trove can tell apart is Polish. The interface
//! reads the same answer through `get_locale`, so the menu bar and the windows
//! never disagree.

use std::sync::OnceLock;

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

/// The locale for this run, read once.
pub fn current() -> Locale {
    static CURRENT: OnceLock<Locale> = OnceLock::new();
    *CURRENT.get_or_init(|| detect(&system_languages()))
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
    fn the_codes_match_the_interface_locales() {
        assert_eq!(Locale::En.code(), "en");
        assert_eq!(Locale::Pl.code(), "pl");
    }
}
