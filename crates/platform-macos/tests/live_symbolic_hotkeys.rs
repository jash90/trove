//! Reads the machine's own shortcut table.
//!
//! Ignored by default: it depends on the state of the machine it runs on, which
//! is exactly what makes it worth having and exactly what makes it a bad
//! quality gate. Run it by hand with
//! `cargo test -p platform-macos --test live_symbolic_hotkeys -- --ignored`
//! when the parsing needs checking against what macOS is actually writing
//! today, rather than against what it wrote when the fixtures were captured.

#[test]
#[ignore = "reads the state of the machine it runs on"]
fn the_live_table_parses_and_answers_who_holds_command_space() {
    let entries = platform_macos::symbolic_hotkeys::read();
    assert!(
        !entries.is_empty(),
        "the system always has a shortcut table; an empty read means the parsing missed it"
    );

    let holders = platform_macos::holders_of(platform_macos::COMMAND_SPACE);
    println!("entries: {}", entries.len());
    println!("holders of Cmd+Space: {holders:?}");
    for id in [60, 61, 64, 65] {
        let entry = entries.iter().find(|entry| entry.id == id);
        println!("  {id}: {entry:?}");
    }
}

/// Turns Spotlight's Cmd+Space off and straight back on again.
///
/// Ignored by default and deliberately net-zero: it leaves the machine holding
/// whatever it held before. What it proves is the part no fixture can — that
/// `defaults` accepts the entry this crate renders, that the system reads it
/// back changed, and that the round trip does not lose the chord the row
/// carries.
#[test]
#[ignore = "writes to the machine's own shortcut table"]
fn the_write_path_changes_the_system_and_can_be_undone() {
    use platform_macos::symbolic_hotkeys::{self, SetOutcome};

    const SPOTLIGHT: i64 = 64;
    let before = symbolic_hotkeys::read()
        .into_iter()
        .find(|entry| entry.id == SPOTLIGHT)
        .expect("the machine must have a Spotlight row to put back");

    let flip = |enabled: bool| {
        let outcome = symbolic_hotkeys::set_enabled(&[SPOTLIGHT], enabled);
        assert!(
            matches!(outcome, SetOutcome::Applied | SetOutcome::NeedsLogout),
            "the write was refused: {outcome:?}"
        );
        symbolic_hotkeys::read()
            .into_iter()
            .find(|entry| entry.id == SPOTLIGHT)
            .expect("the row must survive being written")
    };

    let turned_on = flip(true);
    assert!(turned_on.enabled);
    assert_eq!(
        turned_on.parameters, before.parameters,
        "rewriting the row must not lose the chord it carries"
    );
    assert_eq!(
        symbolic_hotkeys::conflicting_ids(&[turned_on], platform_macos::COMMAND_SPACE),
        vec![SPOTLIGHT],
        "an enabled Spotlight row is exactly the conflict this exists to find"
    );

    // Back to whatever the machine had, whichever way round that was.
    let restored = flip(before.enabled);
    assert_eq!(restored, before);
}
