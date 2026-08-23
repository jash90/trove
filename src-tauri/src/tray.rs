//! The menu bar item.
//!
//! A clipboard manager runs all day with its window hidden, so the menu bar is
//! the only place it exists on screen. It answers the two questions the user
//! actually has — is it still recording, and how do I stop it — and gets out of
//! the way.

use tauri::{
    AppHandle, Emitter, Manager, Runtime,
    menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem},
    tray::{TrayIconBuilder, TrayIconEvent},
};

use crate::monitor::MonitorControl;

const SHOW_ID: &str = "show";
const SETTINGS_ID: &str = "settings";
const PAUSE_ID: &str = "pause";
const QUIT_ID: &str = "quit";

/// Label for the pause entry, which doubles as the recording indicator.
///
/// One entry rather than a separate status line: the wording already answers
/// "is it recording", and a second read-only row would only repeat it.
pub fn pause_label(paused: bool) -> &'static str {
    if paused {
        "Wznów nasłuch"
    } else {
        "Wstrzymaj nasłuch"
    }
}

pub fn install<R: Runtime>(app: &AppHandle<R>, control: MonitorControl) -> tauri::Result<()> {
    let show = MenuItem::with_id(app, SHOW_ID, "Pokaż historię", true, None::<&str>)?;
    let settings = MenuItem::with_id(app, SETTINGS_ID, "Ustawienia…", true, None::<&str>)?;
    let pause = MenuItem::with_id(
        app,
        PAUSE_ID,
        pause_label(control.is_paused()),
        true,
        None::<&str>,
    )?;
    let quit = MenuItem::with_id(app, QUIT_ID, "Zakończ", true, None::<&str>)?;
    let separator = PredefinedMenuItem::separator(app)?;
    let menu = Menu::with_items(app, &[&show, &settings, &pause, &separator, &quit])?;

    let menu_control = control.clone();
    let pause_item = pause.clone();
    TrayIconBuilder::with_id("clipboard-history")
        .icon(app.default_window_icon().cloned().ok_or_else(|| {
            tauri::Error::Anyhow(anyhow::anyhow!("the application has no icon to show"))
        })?)
        .icon_as_template(true)
        .tooltip("Historia schowka")
        .menu(&menu)
        // The menu belongs to the right button. A left click should do the
        // obvious thing — show the history — not open a menu to get there.
        .show_menu_on_left_click(false)
        .on_menu_event(move |app, event| {
            on_menu_event(app, &event, &menu_control, &pause_item);
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click { .. } = event {
                crate::hotkey::toggle_palette(tray.app_handle());
            }
        })
        .build(app)?;
    Ok(())
}

fn on_menu_event<R: Runtime>(
    app: &AppHandle<R>,
    event: &MenuEvent,
    control: &MonitorControl,
    pause_item: &MenuItem<R>,
) {
    match event.id().as_ref() {
        SHOW_ID => show_palette(app),
        SETTINGS_ID => {
            show_palette(app);
            // The window owns its own navigation; the tray only asks.
            let _ = app.emit_to("main", crate::monitor::OPEN_SETTINGS_EVENT, ());
        }
        PAUSE_ID => {
            let paused = !control.is_paused();
            control.set_paused(paused);
            let _ = pause_item.set_text(pause_label(paused));
        }
        QUIT_ID => app.exit(0),
        _ => {}
    }
}

fn show_palette<R: Runtime>(app: &AppHandle<R>) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.set_focus();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_pause_entry_says_what_pressing_it_will_do() {
        // Reading "Wstrzymaj" tells the user it is recording now, so a separate
        // status row would only say the same thing twice.
        assert_eq!(pause_label(false), "Wstrzymaj nasłuch");
        assert_eq!(pause_label(true), "Wznów nasłuch");
    }
}
