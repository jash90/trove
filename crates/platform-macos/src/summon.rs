//! How a summoned window comes forward: on the Space and the screen the user
//! is on, and already floating by the time it is ordered in.
//!
//! All of it is a property of the `NSWindow`, and all of it has to be set on
//! the main thread. It lives here rather than in the application because the
//! runtime's own level setter dispatches asynchronously: called right before
//! `show`, the window was ordered in at the normal level and only floated a
//! moment later — long enough to flash under whatever it was summoned over.

/// Whether a summoned window should float above other applications' windows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Level {
    Floating,
    Normal,
}

/// A rectangle in the screen space AppKit uses: origin bottom-left, in points.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

impl Rect {
    fn contains(&self, x: f64, y: f64) -> bool {
        // Inclusive on every edge: the pointer pinned against the top of the
        // menu bar sits exactly on the screen's maximum y, and it is still
        // that screen the user is looking at.
        x >= self.x && x <= self.x + self.width && y >= self.y && y <= self.y + self.height
    }
}

/// The first screen whose frame holds the point, by index.
pub fn screen_containing(screens: &[Rect], x: f64, y: f64) -> Option<usize> {
    screens.iter().position(|screen| screen.contains(x, y))
}

/// Where a window of this size goes to sit in the middle of `area`.
///
/// A window larger than the area keeps its top-left corner inside it rather
/// than being centred off both edges: the title bar is the one part that has
/// to stay reachable, and in AppKit's bottom-left space that is the top.
pub fn centered_origin(area: Rect, width: f64, height: f64) -> (f64, f64) {
    let x = if width >= area.width {
        area.x
    } else {
        area.x + ((area.width - width) / 2.0).floor()
    };
    let y = if height >= area.height {
        area.y + area.height - height
    } else {
        area.y + ((area.height - height) / 2.0).floor()
    };
    (x, y)
}

#[cfg(target_os = "macos")]
mod platform {
    use objc2_app_kit::{
        NSEvent, NSFloatingWindowLevel, NSNormalWindowLevel, NSScreen, NSWindow,
        NSWindowCollectionBehavior,
    };
    use objc2_foundation::{NSPoint, NSRect};

    use super::{Level, Rect, centered_origin, screen_containing};

    fn rect(frame: NSRect) -> Rect {
        Rect {
            x: frame.origin.x,
            y: frame.origin.y,
            width: frame.size.width,
            height: frame.size.height,
        }
    }

    /// The window behind the raw pointer the runtime hands out, if this is the
    /// main thread and the pointer is a window at all.
    ///
    /// # Safety
    ///
    /// `ns_window` must be null or a live `NSWindow` owned by this process.
    unsafe fn window<'a>(ns_window: *mut std::ffi::c_void) -> Option<&'a NSWindow> {
        objc2::MainThreadMarker::new()?;
        // SAFETY: the caller promises a live NSWindow or null, and the
        // marker above proves this is the thread AppKit allows it on.
        unsafe { ns_window.cast::<NSWindow>().as_ref() }
    }

    /// Makes the window follow the user to whichever Space they are on, and
    /// allows it over a full-screen application.
    ///
    /// Without `MoveToActiveSpace`, showing a window that was last open on
    /// another desktop switches the user to that desktop instead — the
    /// palette dragged them away from the work it was summoned to help with.
    /// `FullScreenAuxiliary` is what lets it appear over a full-screen app at
    /// all rather than on a desktop of its own. `CanJoinAllSpaces` is cleared
    /// because AppKit refuses the combination with `MoveToActiveSpace`.
    ///
    /// Returns false off the main thread or for a null pointer.
    ///
    /// # Safety
    ///
    /// `ns_window` must be null or a live `NSWindow` owned by this process.
    pub unsafe fn follow_active_space(ns_window: *mut std::ffi::c_void) -> bool {
        // SAFETY: forwarded from this function's own contract.
        let Some(window) = (unsafe { window(ns_window) }) else {
            return false;
        };
        let behavior = (window.collectionBehavior() - NSWindowCollectionBehavior::CanJoinAllSpaces)
            | NSWindowCollectionBehavior::MoveToActiveSpace
            | NSWindowCollectionBehavior::FullScreenAuxiliary;
        window.setCollectionBehavior(behavior);
        true
    }

    /// Sets the window's level now, on this thread, rather than queueing it.
    ///
    /// Returns false off the main thread or for a null pointer.
    ///
    /// # Safety
    ///
    /// `ns_window` must be null or a live `NSWindow` owned by this process.
    pub unsafe fn set_level(ns_window: *mut std::ffi::c_void, level: Level) -> bool {
        // SAFETY: forwarded from this function's own contract.
        let Some(window) = (unsafe { window(ns_window) }) else {
            return false;
        };
        window.setLevel(match level {
            Level::Floating => NSFloatingWindowLevel,
            Level::Normal => NSNormalWindowLevel,
        });
        true
    }

    /// Moves the window to the middle of the screen the pointer is on, unless
    /// it is already on that screen.
    ///
    /// A window keeps its frame between openings, so on a second display the
    /// palette came back on whichever screen it was last used on — often not
    /// the one the user was looking at when they asked for it. A window that is
    /// already on the right screen is left where the user put it.
    ///
    /// Returns whether the window was moved.
    ///
    /// # Safety
    ///
    /// `ns_window` must be null or a live `NSWindow` owned by this process.
    pub unsafe fn move_to_pointer_screen(ns_window: *mut std::ffi::c_void) -> bool {
        let Some(mtm) = objc2::MainThreadMarker::new() else {
            return false;
        };
        // SAFETY: forwarded from this function's own contract.
        let Some(window) = (unsafe { window(ns_window) }) else {
            return false;
        };
        let pointer = NSEvent::mouseLocation();
        let screens: Vec<_> = NSScreen::screens(mtm).iter().collect();
        let frames: Vec<Rect> = screens.iter().map(|screen| rect(screen.frame())).collect();
        let Some(index) = screen_containing(&frames, pointer.x, pointer.y) else {
            return false;
        };
        if window
            .screen()
            .is_some_and(|current| rect(current.frame()) == frames[index])
        {
            return false;
        }
        let size = window.frame().size;
        let (x, y) = centered_origin(rect(screens[index].visibleFrame()), size.width, size.height);
        window.setFrameOrigin(NSPoint::new(x, y));
        true
    }
}

#[cfg(target_os = "macos")]
pub use platform::{follow_active_space, move_to_pointer_screen, set_level};

#[cfg(test)]
mod tests {
    use super::*;

    const LEFT: Rect = Rect {
        x: -1920.0,
        y: 0.0,
        width: 1920.0,
        height: 1080.0,
    };
    const MAIN: Rect = Rect {
        x: 0.0,
        y: 0.0,
        width: 1512.0,
        height: 982.0,
    };

    #[test]
    fn the_pointer_picks_the_screen_it_is_on() {
        let screens = [MAIN, LEFT];
        assert_eq!(screen_containing(&screens, 100.0, 100.0), Some(0));
        assert_eq!(screen_containing(&screens, -100.0, 500.0), Some(1));
        // Pinned against the top of the menu bar is still on that screen.
        assert_eq!(screen_containing(&screens, 10.0, 982.0), Some(0));
        assert_eq!(screen_containing(&screens, 5000.0, 5000.0), None);
    }

    #[test]
    fn a_window_is_centred_in_the_area_it_is_given() {
        let area = Rect {
            x: -1920.0,
            y: 40.0,
            width: 1920.0,
            height: 1000.0,
        };
        assert_eq!(centered_origin(area, 1040.0, 680.0), (-1480.0, 200.0));
    }

    #[test]
    fn a_window_larger_than_the_screen_keeps_its_title_bar_on_it() {
        let area = Rect {
            x: 0.0,
            y: 0.0,
            width: 800.0,
            height: 600.0,
        };
        // Its left edge on the area's left edge, its top on the area's top.
        assert_eq!(centered_origin(area, 1040.0, 680.0), (0.0, -80.0));
    }
}
