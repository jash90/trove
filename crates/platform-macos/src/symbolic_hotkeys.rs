//! The shortcuts macOS keeps for itself.
//!
//! A global shortcut registered by an application is dispatched by Carbon, and
//! Carbon never sees a chord the system claimed first. Command-Space is the
//! clearest case: Spotlight owns it, so registering it succeeds and then
//! nothing happens, which reads as the application being broken. There is no
//! way to intercept such a chord — even Alfred tells its users to go and turn
//! Spotlight's shortcut off by hand. The only honest route to "Command-Space
//! summons the palette" is to free the chord first.
//!
//! The system's table lives in one preference domain, so both halves of that —
//! finding out who holds a chord, and handing it back — are reads and writes of
//! `com.apple.symbolichotkeys`. Everything here that decides anything is a pure
//! function over a parsed plist; the parts that touch the machine are three
//! process calls with argument vectors and no shell, the same discipline the
//! paste path already follows.

/// Where the system keeps the shortcuts it dispatches before any application.
const DOMAIN: &str = "com.apple.symbolichotkeys";

/// The one key in that domain worth reading.
const TABLE_KEY: &str = "AppleSymbolicHotKeys";

/// What makes a changed shortcut take effect without logging out.
///
/// Not a documented interface, which is why nothing here depends on it
/// succeeding: the preference is already written by the time it runs, so a
/// missing or failing binary costs the user a logout, not the change.
const ACTIVATE_SETTINGS: &str =
    "/System/Library/PrivateFrameworks/SystemAdministration.framework/Resources/activateSettings";

/// Modifier masks as the preference spells them.
pub const MODIFIER_SHIFT: i64 = 131_072;
pub const MODIFIER_CONTROL: i64 = 262_144;
pub const MODIFIER_OPTION: i64 = 524_288;
pub const MODIFIER_COMMAND: i64 = 1_048_576;

/// The half of a modifier mask that says which key, rather than which side.
///
/// The low bits distinguish the left Command key from the right one. Two rows
/// that differ only there are the same shortcut to anybody pressing it, so they
/// are masked off before anything is compared.
const DEVICE_INDEPENDENT: i64 = 0xFFFF_0000;

/// Virtual key code for Space, which is what every chord here is built on.
const KEY_SPACE: i64 = 49;

/// The character Space carries in the parameter triple.
const CHARACTER_SPACE: i64 = 32;

/// A chord, as the physical keys that produce it.
///
/// The parameter triple the preference stores also carries a character, but the
/// character is derived from the other two and differs between layouts. Two
/// entries with the same key code and modifiers are the same chord whatever
/// character they claim, so that is what equality means here.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Chord {
    pub key_code: i64,
    pub modifiers: i64,
}

/// The chord this application wants.
pub const COMMAND_SPACE: Chord = Chord {
    key_code: KEY_SPACE,
    modifiers: MODIFIER_COMMAND,
};

/// One row of the system's table, kept whole so rewriting one is lossless.
///
/// `parameters` is the raw triple — character, key code, modifiers — because
/// writing an entry back has to hand the system the same shape it stores, and
/// dropping the character on the way in would mean inventing one on the way out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SymbolicHotKey {
    pub id: i64,
    pub enabled: bool,
    pub parameters: Option<[i64; 3]>,
}

impl SymbolicHotKey {
    /// The chord this row answers to, when it carries one at all.
    pub fn chord(&self) -> Option<Chord> {
        self.parameters.map(|[_, key_code, modifiers]| Chord {
            key_code,
            modifiers: modifiers & DEVICE_INDEPENDENT,
        })
    }
}

/// A system shortcut as it arrives from the factory.
///
/// Needed because macOS omits from the preference every entry still standing at
/// its default: an absent id is an *enabled* shortcut, not a missing one, and a
/// reader that treats absence as "nobody holds this" concludes the opposite of
/// the truth on a machine nobody has touched.
struct FactorySetting {
    id: i64,
    parameters: [i64; 3],
}

/// The system shortcuts built on Space — the only ones that can stand on the
/// chord this application wants.
const FACTORY_SETTINGS: &[FactorySetting] = &[
    // Select the previous input source — Control-Space.
    FactorySetting {
        id: 60,
        parameters: [CHARACTER_SPACE, KEY_SPACE, MODIFIER_CONTROL],
    },
    // Select the next source in the input menu — Control-Option-Space.
    FactorySetting {
        id: 61,
        parameters: [
            CHARACTER_SPACE,
            KEY_SPACE,
            MODIFIER_CONTROL | MODIFIER_OPTION,
        ],
    },
    // Show Spotlight search — Command-Space.
    FactorySetting {
        id: 64,
        parameters: [CHARACTER_SPACE, KEY_SPACE, MODIFIER_COMMAND],
    },
    // Show the Finder search window — Option-Command-Space.
    FactorySetting {
        id: 65,
        parameters: [
            CHARACTER_SPACE,
            KEY_SPACE,
            MODIFIER_OPTION | MODIFIER_COMMAND,
        ],
    },
];

/// How a request to change the system's table actually went.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SetOutcome {
    /// Written, and the system reloaded its table: the change is live.
    Applied,
    /// Written, but the reload did not run. The change stands after a logout.
    NeedsLogout,
    /// Nothing was written and nothing changed.
    Failed,
}

/// Reads the system's table out of a parsed preference domain.
///
/// Rows the system spells in a shape this does not recognise are dropped rather
/// than guessed at: a row read wrongly would be a row disabled wrongly.
pub fn parse_table(domain: &plist::Value) -> Vec<SymbolicHotKey> {
    let Some(table) = domain
        .as_dictionary()
        .and_then(|domain| domain.get(TABLE_KEY))
        .and_then(plist::Value::as_dictionary)
    else {
        return Vec::new();
    };
    let mut entries: Vec<SymbolicHotKey> = table
        .iter()
        .filter_map(|(id, entry)| {
            let id = id.parse::<i64>().ok()?;
            let entry = entry.as_dictionary()?;
            // An entry with no `enabled` key is an enabled one: the system
            // writes the flag when it turns something off, not when it leaves
            // it alone.
            let enabled = entry
                .get("enabled")
                .and_then(plist::Value::as_boolean)
                .unwrap_or(true);
            Some(SymbolicHotKey {
                id,
                enabled,
                parameters: parameters_of(entry),
            })
        })
        .collect();
    entries.sort_unstable_by_key(|entry| entry.id);
    entries
}

fn parameters_of(entry: &plist::Dictionary) -> Option<[i64; 3]> {
    let values = entry
        .get("value")
        .and_then(plist::Value::as_dictionary)?
        .get("parameters")
        .and_then(plist::Value::as_array)?;
    let [character, key_code, modifiers] = values.as_slice() else {
        return None;
    };
    Some([
        character.as_signed_integer()?,
        key_code.as_signed_integer()?,
        modifiers.as_signed_integer()?,
    ])
}

/// Which system shortcuts stand between the user and `wanted`.
///
/// Two ways a shortcut can be in the way, and only one of them is visible in
/// the preference: a row that is present, enabled and carries the chord, or a
/// factory setting on that chord whose row was never written because nobody
/// ever changed it.
pub fn conflicting_ids(entries: &[SymbolicHotKey], wanted: Chord) -> Vec<i64> {
    let mut ids: Vec<i64> = entries
        .iter()
        .filter(|entry| entry.enabled && entry.chord() == Some(wanted))
        .map(|entry| entry.id)
        .collect();
    for factory in FACTORY_SETTINGS {
        let absent = !entries.iter().any(|entry| entry.id == factory.id);
        let [_, key_code, modifiers] = factory.parameters;
        let carries_wanted = Chord {
            key_code,
            modifiers: modifiers & DEVICE_INDEPENDENT,
        } == wanted;
        if absent && carries_wanted {
            ids.push(factory.id);
        }
    }
    ids.sort_unstable();
    ids.dedup();
    ids
}

/// The parameters to write for one id, preferring what the system already has.
///
/// Rewriting a row means handing back its own `value`; only a row that was
/// never written needs the factory triple, and an id this crate knows nothing
/// about gets no invented one.
fn parameters_for(entries: &[SymbolicHotKey], id: i64) -> Option<[i64; 3]> {
    entries
        .iter()
        .find(|entry| entry.id == id)
        .and_then(|entry| entry.parameters)
        .or_else(|| {
            FACTORY_SETTINGS
                .iter()
                .find(|factory| factory.id == id)
                .map(|factory| factory.parameters)
        })
}

/// One entry rendered the way `defaults ... -dict-add` wants to be handed it.
///
/// A whole entry rather than just the flag, because `-dict-add` replaces the
/// value it is given a key for — writing `enabled` alone would drop the chord
/// the row carries and leave the system a shortcut bound to nothing.
fn entry_plist(enabled: bool, parameters: [i64; 3]) -> String {
    let [character, key_code, modifiers] = parameters;
    let flag = if enabled { "<true/>" } else { "<false/>" };
    format!(
        "<dict><key>enabled</key>{flag}<key>value</key><dict><key>parameters</key>\
         <array><integer>{character}</integer><integer>{key_code}</integer>\
         <integer>{modifiers}</integer></array><key>type</key><string>standard</string>\
         </dict></dict>"
    )
}

#[cfg(target_os = "macos")]
mod platform {
    use std::process::Command;

    use super::{
        ACTIVATE_SETTINGS, DOMAIN, SetOutcome, SymbolicHotKey, TABLE_KEY, entry_plist,
        parameters_for, parse_table,
    };

    /// The system's table as it stands.
    ///
    /// Read through `defaults` rather than off the plist file: the file on disk
    /// is whatever `cfprefsd` last flushed, and a stale read here would report a
    /// shortcut as free while the system still dispatches it.
    pub fn read() -> Vec<SymbolicHotKey> {
        let Ok(exported) = Command::new("/usr/bin/defaults")
            .args(["export", DOMAIN, "-"])
            .output()
        else {
            return Vec::new();
        };
        if !exported.status.success() {
            return Vec::new();
        }
        plist::Value::from_reader_xml(exported.stdout.as_slice())
            .map(|domain| parse_table(&domain))
            .unwrap_or_default()
    }

    /// Turns the listed system shortcuts off, or hands them back.
    ///
    /// Written one id at a time with `-dict-add` rather than by importing the
    /// whole domain: replacing the table wholesale would take every other
    /// shortcut on the machine with it if anything had changed since the read.
    pub fn set_enabled(ids: &[i64], enabled: bool) -> SetOutcome {
        if ids.is_empty() {
            return SetOutcome::Applied;
        }
        let entries = read();
        let mut written = false;
        for id in ids {
            let Some(parameters) = parameters_for(&entries, *id) else {
                continue;
            };
            let wrote = Command::new("/usr/bin/defaults")
                .args([
                    "write",
                    DOMAIN,
                    TABLE_KEY,
                    "-dict-add",
                    &id.to_string(),
                    &entry_plist(enabled, parameters),
                ])
                .status()
                .map(|status| status.success())
                .unwrap_or(false);
            written |= wrote;
        }
        if !written {
            return SetOutcome::Failed;
        }
        if reload() {
            SetOutcome::Applied
        } else {
            SetOutcome::NeedsLogout
        }
    }

    /// Opens the Keyboard shortcut list in System Settings.
    ///
    /// The reliable half of asking, exactly as the Accessibility path already
    /// works: when this application cannot free a chord itself, the user still
    /// has to land on the one list that decides it, not be told to go looking.
    pub fn open_keyboard_shortcut_settings() -> bool {
        // A single argument, never through a shell, like every other `open` in
        // this application.
        Command::new("/usr/bin/open")
            .arg("x-apple.systempreferences:com.apple.Keyboard-Settings.extension")
            .status()
            .map(|status| status.success())
            .unwrap_or(false)
    }

    /// Asks the system to pick the table up now instead of at the next login.
    fn reload() -> bool {
        Command::new(ACTIVATE_SETTINGS)
            .arg("-u")
            .status()
            .map(|status| status.success())
            .unwrap_or(false)
    }
}

#[cfg(target_os = "macos")]
pub use platform::{open_keyboard_shortcut_settings, read, set_enabled};

#[cfg(not(target_os = "macos"))]
pub fn read() -> Vec<SymbolicHotKey> {
    Vec::new()
}

#[cfg(not(target_os = "macos"))]
pub fn set_enabled(_ids: &[i64], _enabled: bool) -> SetOutcome {
    SetOutcome::Failed
}

#[cfg(not(target_os = "macos"))]
pub fn open_keyboard_shortcut_settings() -> bool {
    false
}

/// Which system shortcuts hold `chord` right now.
pub fn holders_of(chord: Chord) -> Vec<i64> {
    conflicting_ids(&read(), chord)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(id: i64, enabled: bool, modifiers: i64) -> SymbolicHotKey {
        SymbolicHotKey {
            id,
            enabled,
            parameters: Some([CHARACTER_SPACE, KEY_SPACE, modifiers]),
        }
    }

    #[test]
    fn spotlight_holding_command_space_is_the_conflict() {
        let entries = [entry(64, true, MODIFIER_COMMAND)];
        assert_eq!(conflicting_ids(&entries, COMMAND_SPACE), vec![64]);
    }

    #[test]
    fn a_shortcut_already_turned_off_is_not_in_the_way() {
        let entries = [entry(64, false, MODIFIER_COMMAND)];
        assert!(conflicting_ids(&entries, COMMAND_SPACE).is_empty());
    }

    #[test]
    fn an_absent_entry_is_an_enabled_one() {
        // macOS omits every row still standing at its factory setting, so a
        // table with no entry 64 is a machine where Spotlight holds
        // Command-Space — the opposite of what "not listed" reads as.
        let entries = [entry(60, true, MODIFIER_CONTROL)];
        assert_eq!(conflicting_ids(&entries, COMMAND_SPACE), vec![64]);
    }

    #[test]
    fn another_chord_on_the_same_key_is_left_alone() {
        // Control-Space and Control-Option-Space are also Space shortcuts, and
        // disabling them to free Command-Space would take the input-source
        // switcher away for nothing.
        let entries = [
            entry(60, true, MODIFIER_CONTROL),
            entry(61, true, MODIFIER_CONTROL | MODIFIER_OPTION),
            entry(64, true, MODIFIER_COMMAND),
            entry(65, true, MODIFIER_OPTION | MODIFIER_COMMAND),
        ];
        assert_eq!(conflicting_ids(&entries, COMMAND_SPACE), vec![64]);
    }

    #[test]
    fn a_row_remapped_onto_command_space_counts_too() {
        // The Finder search window, moved onto Command-Space by hand, holds the
        // chord exactly as firmly as Spotlight would.
        let entries = [
            entry(64, false, MODIFIER_COMMAND),
            entry(65, true, MODIFIER_COMMAND),
        ];
        assert_eq!(conflicting_ids(&entries, COMMAND_SPACE), vec![65]);
    }

    #[test]
    fn an_id_this_crate_never_heard_of_still_counts_when_it_carries_the_chord() {
        // The table is scanned rather than consulted against a list of known
        // ids, because the ids do not mean what the documentation says they do:
        // on a live macOS 26 machine entry 65 carries [92, 42, 1441792], not
        // the Option-Command-Space it is supposed to. Anything can end up on
        // any chord, so what an entry carries is the only thing worth reading.
        let entries = [
            entry(64, false, MODIFIER_COMMAND),
            SymbolicHotKey {
                id: 175,
                enabled: true,
                parameters: Some([CHARACTER_SPACE, KEY_SPACE, MODIFIER_COMMAND]),
            },
        ];
        assert_eq!(conflicting_ids(&entries, COMMAND_SPACE), vec![175]);

        // And with Spotlight's own row absent, both are in the way: the one
        // that says so and the one that says nothing because it never moved.
        let entries = [SymbolicHotKey {
            id: 175,
            enabled: true,
            parameters: Some([CHARACTER_SPACE, KEY_SPACE, MODIFIER_COMMAND]),
        }];
        assert_eq!(conflicting_ids(&entries, COMMAND_SPACE), vec![64, 175]);
    }

    #[test]
    fn which_command_key_was_pressed_does_not_change_the_chord() {
        // The low half of the mask says left or right; nobody pressing the
        // shortcut means one of them, so a row carrying those bits is the same
        // shortcut and must still be found.
        let entries = [SymbolicHotKey {
            id: 64,
            enabled: true,
            parameters: Some([CHARACTER_SPACE, KEY_SPACE, MODIFIER_COMMAND | 0x8]),
        }];
        assert_eq!(conflicting_ids(&entries, COMMAND_SPACE), vec![64]);
    }

    #[test]
    fn a_row_shaped_wrongly_is_dropped_rather_than_guessed_at() {
        // This dictionary is written by every version of macOS and by every
        // utility that has ever rebound a key. A row read wrongly would be a
        // row disabled wrongly, so an unreadable one carries no chord at all.
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0">
<dict>
  <key>AppleSymbolicHotKeys</key>
  <dict>
    <key>64</key>
    <dict>
      <key>enabled</key><true/>
      <key>value</key>
      <dict>
        <key>parameters</key>
        <array><integer>32</integer><integer>49</integer></array>
        <key>type</key><string>standard</string>
      </dict>
    </dict>
    <key>not-a-number</key>
    <dict><key>enabled</key><true/></dict>
  </dict>
</dict>
</plist>"#;
        let entries = parse_table(&plist::Value::from_reader_xml(xml.as_bytes()).unwrap());

        // The short triple leaves 64 without a chord, and the non-numeric id is
        // gone entirely.
        assert_eq!(
            entries,
            vec![SymbolicHotKey {
                id: 64,
                enabled: true,
                parameters: None
            }]
        );
        // 64 is present, so it is not defaulted back in, and it carries nothing
        // to collide with.
        assert!(conflicting_ids(&entries, COMMAND_SPACE).is_empty());
    }

    #[test]
    fn the_preference_is_read_the_way_the_system_writes_it() {
        // Shaped exactly like `defaults export com.apple.symbolichotkeys -`,
        // including the row with no `enabled` key at all.
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>AppleSymbolicHotKeys</key>
  <dict>
    <key>64</key>
    <dict>
      <key>enabled</key><false/>
      <key>value</key>
      <dict>
        <key>parameters</key>
        <array><integer>32</integer><integer>49</integer><integer>1048576</integer></array>
        <key>type</key><string>standard</string>
      </dict>
    </dict>
    <key>65</key>
    <dict>
      <key>value</key>
      <dict>
        <key>parameters</key>
        <array><integer>32</integer><integer>49</integer><integer>1572864</integer></array>
        <key>type</key><string>standard</string>
      </dict>
    </dict>
  </dict>
</dict>
</plist>"#;
        let domain = plist::Value::from_reader_xml(xml.as_bytes()).unwrap();
        let entries = parse_table(&domain);

        assert_eq!(
            entries,
            vec![
                SymbolicHotKey {
                    id: 64,
                    enabled: false,
                    parameters: Some([32, 49, 1_048_576])
                },
                SymbolicHotKey {
                    id: 65,
                    enabled: true,
                    parameters: Some([32, 49, 1_572_864])
                },
            ]
        );
        // 64 is off and 65 sits on Option-Command-Space, so nothing holds the
        // chord — and 65 being present means it is not defaulted back in.
        assert!(conflicting_ids(&entries, COMMAND_SPACE).is_empty());
    }

    #[test]
    fn a_domain_without_the_table_reads_as_empty_rather_than_failing() {
        let domain = plist::Value::Dictionary(plist::Dictionary::new());
        assert!(parse_table(&domain).is_empty());
    }

    #[test]
    fn rewriting_a_row_keeps_the_chord_it_already_carried() {
        // `-dict-add` replaces the whole entry, so writing the flag alone would
        // leave the system a shortcut bound to nothing.
        let entries = [entry(64, true, MODIFIER_COMMAND)];
        let parameters = parameters_for(&entries, 64).unwrap();
        assert_eq!(parameters, [32, 49, 1_048_576]);

        let rendered = entry_plist(false, parameters);
        assert!(rendered.contains("<false/>"));
        assert!(rendered.contains("<integer>1048576</integer>"));
        assert!(rendered.contains("<string>standard</string>"));
    }

    #[test]
    fn a_row_that_was_never_written_is_written_from_its_factory_setting() {
        assert_eq!(parameters_for(&[], 64), Some([32, 49, 1_048_576]));
        // An id this crate knows nothing about gets no invented chord.
        assert_eq!(parameters_for(&[], 9_999), None);
    }
}
