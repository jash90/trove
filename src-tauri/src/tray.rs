//! The menu bar item.
//!
//! A clipboard manager runs all day with its window hidden, so the menu bar is
//! the only place it exists on screen. It answers the two questions the user
//! actually has — is it still recording, and how do I stop it — and gets out of
//! the way.

use tauri::{
    AppHandle, Manager, Runtime,
    image::Image,
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
        "Resume capture"
    } else {
        "Pause capture"
    }
}

pub fn install<R: Runtime>(app: &AppHandle<R>, control: MonitorControl) -> tauri::Result<()> {
    let show = MenuItem::with_id(app, SHOW_ID, "Show history", true, None::<&str>)?;
    let settings = MenuItem::with_id(app, SETTINGS_ID, "Settings…", true, None::<&str>)?;
    let pause = MenuItem::with_id(
        app,
        PAUSE_ID,
        pause_label(control.is_paused()),
        true,
        None::<&str>,
    )?;
    let quit = MenuItem::with_id(app, QUIT_ID, "Quit", true, None::<&str>)?;
    let separator = PredefinedMenuItem::separator(app)?;
    let menu = Menu::with_items(app, &[&show, &settings, &pause, &separator, &quit])?;

    let menu_control = control.clone();
    let pause_item = pause.clone();
    TrayIconBuilder::with_id("clipboard-history")
        // Its own drawing, not the application icon. A template icon is drawn
        // from its alpha channel alone — every opaque pixel becomes one flat
        // colour — so the coloured icon that suits the Dock arrives in the menu
        // bar as a featureless rectangle. This one is a glyph: mostly
        // transparent, so what survives the flattening is the clipboard.
        .icon(Image::from_bytes(include_bytes!("../icons/tray.png"))?)
        .icon_as_template(true)
        .tooltip("Clipboard history")
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
        SETTINGS_ID => crate::hotkey::show_settings(app),
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
        assert_eq!(pause_label(false), "Pause capture");
        assert_eq!(pause_label(true), "Resume capture");
    }
}
