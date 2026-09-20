//! Moving other applications' windows, the way Rectangle does.
//!
//! The keyboard shortcuts this answers are global, but the act itself is
//! native: the Accessibility API is the one sanctioned way to ask another
//! application's window to move, and it costs the same permission the paste
//! suppression already asks for — there is no second dialog to survive.
//!
//! The half of this module that decides anything is pure geometry in
//! top-left-origin coordinates, so it is tested on every host the workspace
//! checks on; only the calls that talk to the system live behind the macOS
//! gate.

/// One snap position, named the way the settings row and the menu agree on.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SnapAction {
    LeftHalf,
    RightHalf,
    TopHalf,
    BottomHalf,
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
    Maximize,
    Center,
}

impl SnapAction {
    /// The settings key for this action, stable across upgrades.
    pub fn id(self) -> &'static str {
        match self {
            Self::LeftHalf => "leftHalf",
            Self::RightHalf => "rightHalf",
            Self::TopHalf => "topHalf",
            Self::BottomHalf => "bottomHalf",
            Self::TopLeft => "topLeft",
            Self::TopRight => "topRight",
            Self::BottomLeft => "bottomLeft",
            Self::BottomRight => "bottomRight",
            Self::Maximize => "maximize",
            Self::Center => "center",
        }
    }

    /// The action a settings key names, if it names one.
    pub fn from_id(id: &str) -> Option<Self> {
        Some(match id {
            "leftHalf" => Self::LeftHalf,
            "rightHalf" => Self::RightHalf,
            "topHalf" => Self::TopHalf,
            "bottomHalf" => Self::BottomHalf,
            "topLeft" => Self::TopLeft,
            "topRight" => Self::TopRight,
            "bottomLeft" => Self::BottomLeft,
            "bottomRight" => Self::BottomRight,
            "maximize" => Self::Maximize,
            "center" => Self::Center,
            _ => return None,
        })
    }
}

/// A rectangle in the Accessibility coordinate system: origin at the top-left
/// of the primary display, y growing downward.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

/// What became of one snap attempt.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SnapOutcome {
    Moved,
    /// The system has not granted Accessibility permission.
    PermissionRequired,
    /// Nothing was in front worth snapping — no frontmost application, or only
    /// this process, whose floating palette is not a window anyone tiles.
    NoTarget,
    /// The target application refused the move.
    Refused,
}

/// Flips a bottom-left-origin AppKit rectangle into top-left-origin
/// Accessibility coordinates. The primary display's full height is the one
/// number the flip needs; everything else is arithmetic.
pub fn ns_to_ax(rect: Rect, primary_height: f64) -> Rect {
    Rect {
        x: rect.x,
        y: primary_height - rect.y - rect.height,
        width: rect.width,
        height: rect.height,
    }
}

/// The area two rectangles overlap, `0.0` when they do not.
fn overlap(a: Rect, b: Rect) -> f64 {
    let width = (a.x + a.width).min(b.x + b.width) - a.x.max(b.x);
    let height = (a.y + a.height).min(b.y + b.height) - a.y.max(b.y);
    width.max(0.0) * height.max(0.0)
}

/// Which screen a window belongs on: the one it covers most of. A window
/// straddling a bezel is on whichever side holds more of it, and a window
/// lost off-screen falls back to the first screen rather than to none.
pub fn screen_for(window: Rect, screens: &[Rect]) -> usize {
    let mut best = 0usize;
    let mut best_area = f64::MIN;
    for (index, screen) in screens.iter().enumerate() {
        let area = overlap(window, *screen);
        if area > best_area {
            best_area = area;
            best = index;
        }
    }
    best
}

/// Where an action puts a window, given the screen area it may use (already
/// excluding the menu bar and Dock) and the window as it stands — centering
/// keeps the window's size, so it is the one action that needs both.
pub fn target_rect(action: SnapAction, visible: Rect, window: Rect) -> Rect {
    let half_width = visible.width / 2.0;
    let half_height = visible.height / 2.0;
    let right = visible.x + half_width;
    let bottom = visible.y + half_height;
    match action {
        SnapAction::LeftHalf => Rect { x: visible.x, y: visible.y, width: half_width, height: visible.height },
        SnapAction::RightHalf => Rect { x: right, y: visible.y, width: half_width, height: visible.height },
        SnapAction::TopHalf => Rect { x: visible.x, y: visible.y, width: visible.width, height: half_height },
        SnapAction::BottomHalf => Rect { x: visible.x, y: bottom, width: visible.width, height: half_height },
        SnapAction::TopLeft => Rect { x: visible.x, y: visible.y, width: half_width, height: half_height },
        SnapAction::TopRight => Rect { x: right, y: visible.y, width: half_width, height: half_height },
        SnapAction::BottomLeft => Rect { x: visible.x, y: bottom, width: half_width, height: half_height },
        SnapAction::BottomRight => Rect { x: right, y: bottom, width: half_width, height: half_height },
        SnapAction::Maximize => visible,
        SnapAction::Center => Rect {
            x: visible.x + (visible.width - window.width) / 2.0,
            y: visible.y + (visible.height - window.height) / 2.0,
            width: window.width,
            height: window.height,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const VISIBLE: Rect = Rect { x: 0.0, y: 25.0, width: 1000.0, height: 775.0 };

    #[test]
    fn halves_split_each_axis_in_order() {
        let left = target_rect(SnapAction::LeftHalf, VISIBLE, VISIBLE);
        assert_eq!(left, Rect { x: 0.0, y: 25.0, width: 500.0, height: 775.0 });
        let right = target_rect(SnapAction::RightHalf, VISIBLE, VISIBLE);
        assert_eq!(right.x, 500.0);
        let top = target_rect(SnapAction::TopHalf, VISIBLE, VISIBLE);
        assert_eq!(top.height, 387.5);
        let bottom = target_rect(SnapAction::BottomHalf, VISIBLE, VISIBLE);
        assert_eq!(bottom.y, 25.0 + 387.5);
    }

    #[test]
    fn quarters_cover_the_four_corners() {
        let tl = target_rect(SnapAction::TopLeft, VISIBLE, VISIBLE);
        let br = target_rect(SnapAction::BottomRight, VISIBLE, VISIBLE);
        assert_eq!((tl.x, tl.y), (0.0, 25.0));
        assert_eq!((br.x, br.y), (500.0, 25.0 + 387.5));
        assert_eq!(tl.width, br.width);
        assert_eq!(tl.height, br.height);
    }

    #[test]
    fn maximize_fills_the_visible_area_and_center_keeps_the_window() {
        assert_eq!(target_rect(SnapAction::Maximize, VISIBLE, VISIBLE), VISIBLE);
        let odd = Rect { x: 900.0, y: 600.0, width: 200.0, height: 150.0 };
        let centered = target_rect(SnapAction::Center, VISIBLE, odd);
        assert_eq!((centered.width, centered.height), (200.0, 150.0));
        assert_eq!(centered.x, (1000.0 - 200.0) / 2.0);
        assert_eq!(centered.y, 25.0 + (775.0 - 150.0) / 2.0);
    }

    #[test]
    fn the_appkit_flip_moves_the_origin_not_the_size() {
        // A menu bar sits at the top of a 1050-point display: visibleFrame
        // reports y=0 height=1025 in AppKit terms, which is y=25 downward.
        let visible_ns = Rect { x: 0.0, y: 0.0, width: 1000.0, height: 1025.0 };
        assert_eq!(
            ns_to_ax(visible_ns, 1050.0),
            Rect { x: 0.0, y: 25.0, width: 1000.0, height: 1025.0 }
        );
    }

    #[test]
    fn a_window_belongs_to_the_screen_it_covers_most_of() {
        let screens = [
            Rect { x: 0.0, y: 0.0, width: 1000.0, height: 800.0 },
            Rect { x: 1000.0, y: 0.0, width: 1000.0, height: 800.0 },
        ];
        let straddling = Rect { x: 700.0, y: 0.0, width: 800.0, height: 800.0 };
        // 300 points on the left screen, 500 on the right: the right wins.
        assert_eq!(screen_for(straddling, &screens), 1);
        let lost = Rect { x: -4000.0, y: -4000.0, width: 100.0, height: 100.0 };
        assert_eq!(screen_for(lost, &screens), 0);
    }

    #[test]
    fn every_action_round_trips_through_its_id() {
        for action in [
            SnapAction::LeftHalf, SnapAction::RightHalf, SnapAction::TopHalf,
            SnapAction::BottomHalf, SnapAction::TopLeft, SnapAction::TopRight,
            SnapAction::BottomLeft, SnapAction::BottomRight, SnapAction::Maximize,
            SnapAction::Center,
        ] {
            assert_eq!(SnapAction::from_id(action.id()), Some(action));
        }
        assert_eq!(SnapAction::from_id("nope"), None);
    }
}

#[cfg(target_os = "macos")]
mod platform {
    
    use objc2_app_kit::{NSScreen, NSWorkspace};

    use super::{Rect, SnapAction, SnapOutcome, ns_to_ax, screen_for, target_rect};

// The C face of the Accessibility API, linked by framework the way the
// trust check in `paste` is. Only the handful of calls a snap needs are
// declared; there is no element tree to walk, only one focused window.
#[link(name = "ApplicationServices", kind = "framework")]
unsafe extern "C" {
fn AXUIElementCreateApplication(pid: i32) -> *const std::ffi::c_void;
fn AXUIElementCopyAttributeValue(
element: *const std::ffi::c_void,
attribute: *const std::ffi::c_void,
value: *mut *const std::ffi::c_void,
) -> i32;
fn AXUIElementSetAttributeValue(
element: *const std::ffi::c_void,
attribute: *const std::ffi::c_void,
value: *const std::ffi::c_void,
) -> i32;
fn AXValueCreate(the_type: u32, value_ptr: *const std::ffi::c_void) -> *const std::ffi::c_void;
fn AXValueGetValue(
value: *const std::ffi::c_void,
the_type: u32,
value_ptr: *mut std::ffi::c_void,
) -> bool;
fn CFRelease(cf: *const std::ffi::c_void);
}

// The attribute-name constants (`kAXPositionAttribute` and friends) are
// no longer exported as linkable symbols by the SDK, so the documented
// string values are built at runtime instead — the same CFStrings the
// constants hold, reached the way every post-deprecation caller does.
#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
fn CFStringCreateWithCString(
allocator: *const std::ffi::c_void,
c_string: *const std::ffi::c_char,
encoding: u32,
) -> *const std::ffi::c_void;
}

const K_CF_STRING_ENCODING_UTF8: u32 = 0x0800_0100;

/// The CFString form of one Accessibility attribute name. Owned by the
/// caller, which releases it when the call it fed is done.
fn cf_string(name: &[u8]) -> *const std::ffi::c_void {
// SAFETY: `name` is NUL-terminated by every caller, and a NULL
// allocator selects the default one.
unsafe {
CFStringCreateWithCString(
std::ptr::null(),
name.as_ptr() as *const std::ffi::c_char,
K_CF_STRING_ENCODING_UTF8,
)
}
}

    const K_AX_VALUE_CG_POINT_TYPE: u32 = 1;
    const K_AX_VALUE_CG_SIZE_TYPE: u32 = 2;

    /// The value layout `AXValueGetValue` copies a position into: two `f64`s,
    /// `repr(C)`, the same ABI `CGPoint` has — declared here rather than
    /// dragging a crate in for sixteen bytes of layout.
    #[repr(C)]
    #[derive(Default)]
    struct AxPoint { x: f64, y: f64 }

    /// The size counterpart, two `f64`s like `CGSize`.
    #[repr(C)]
    #[derive(Default)]
    struct AxSize { width: f64, height: f64 }

    /// Moves the frontmost application's focused window to one snap position.
    ///
    /// Must run on the main thread: the screen list is an AppKit query. The
    /// global-shortcut handler routes this through `run_on_main_thread`, and
    /// an off-main call refuses rather than guessing at screens.
    pub fn snap(action: SnapAction) -> SnapOutcome {
        if !super::super::paste::is_trusted() {
            return SnapOutcome::PermissionRequired;
        }
        let Some(mtm) = objc2::MainThreadMarker::new() else {
            return SnapOutcome::Refused;
        };
        // SAFETY: reading the shared workspace singleton and the frontmost
        // application record is a query with no preconditions.
        let frontmost = unsafe { NSWorkspace::sharedWorkspace().frontmostApplication() };
        let Some(app) = frontmost else { return SnapOutcome::NoTarget };
        // SAFETY: reading a property of an owned running-application record.
        let pid = unsafe { app.processIdentifier() };
        if pid == std::process::id() as i32 {
            return SnapOutcome::NoTarget;
        }
        snap_window_of(mtm, pid, action)
    }

    /// The visible area of each screen, in Accessibility coordinates.
    fn screens_ax(mtm: objc2::MainThreadMarker) -> Vec<Rect> {
        // SAFETY: `screens` is a copy out of the screen list and
        // `visibleFrame` is a plain query, made on the main thread the marker
        // proves we are on.
        unsafe {
            let screens = NSScreen::screens(mtm);
            let list = screens.iter().collect::<Vec<_>>();
            let Some(primary) = list.first() else { return Vec::new() };
            let primary_height = primary.frame().size.height;
            let mut visibles = Vec::with_capacity(list.len());
            for screen in &list {
                let v = screen.visibleFrame();
                let to_rect = |r: objc2_foundation::NSRect| Rect {
                    x: r.origin.x,
                    y: r.origin.y,
                    width: r.size.width,
                    height: r.size.height,
                };
                visibles.push(ns_to_ax(to_rect(v), primary_height));
            }
            visibles
        }
    }

    fn read_point(window: *const std::ffi::c_void) -> Option<(f64, f64)> {
        // SAFETY: the window element was handed back by the Accessibility
        // API, the attribute name is NUL-terminated, and the value buffer is
        // a plain f64 pair.
        unsafe {
            let attribute = cf_string(b"AXPosition\0");
            if attribute.is_null() {
                return None;
            }
            let mut raw: *const std::ffi::c_void = std::ptr::null();
            let copied = AXUIElementCopyAttributeValue(window, attribute, &mut raw);
            CFRelease(attribute);
            if copied != 0 {
                return None;
            }
            let mut point = AxPoint::default();
            let read = AXValueGetValue(
                raw,
                K_AX_VALUE_CG_POINT_TYPE,
                &mut point as *mut _ as *mut std::ffi::c_void,
            );
            CFRelease(raw);
            read.then_some((point.x, point.y))
        }
    }

    fn read_size(window: *const std::ffi::c_void) -> Option<(f64, f64)> {
        // SAFETY: as `read_point`, for the size attribute.
        unsafe {
            let attribute = cf_string(b"AXSize\0");
            if attribute.is_null() {
                return None;
            }
            let mut raw: *const std::ffi::c_void = std::ptr::null();
            let copied = AXUIElementCopyAttributeValue(window, attribute, &mut raw);
            CFRelease(attribute);
            if copied != 0 {
                return None;
            }
            let mut size = AxSize::default();
            let read = AXValueGetValue(
                raw,
                K_AX_VALUE_CG_SIZE_TYPE,
                &mut size as *mut _ as *mut std::ffi::c_void,
            );
            CFRelease(raw);
            read.then_some((size.width, size.height))
        }
    }

    /// Position first, size second: a window that lands mid-resize keeps
    /// moving toward its corner instead of flashing at the old origin.
    fn write_frame(window: *const std::ffi::c_void, target: Rect) -> bool {
        // SAFETY: the values and attribute names are created and released
        // here, and the window element is the one the caller obtained from
        // the focused window.
        unsafe {
            let position = cf_string(b"AXPosition\0");
            let size = cf_string(b"AXSize\0");
            if position.is_null() || size.is_null() {
                if !position.is_null() { CFRelease(position); }
                if !size.is_null() { CFRelease(size); }
                return false;
            }
            let point = AxPoint { x: target.x, y: target.y };
            let ax_size = AxSize { width: target.width, height: target.height };
            let point_value = AXValueCreate(
                K_AX_VALUE_CG_POINT_TYPE,
                &point as *const _ as *const std::ffi::c_void,
            );
            let size_value = AXValueCreate(
                K_AX_VALUE_CG_SIZE_TYPE,
                &ax_size as *const _ as *const std::ffi::c_void,
            );
            if point_value.is_null() || size_value.is_null() {
                if !point_value.is_null() { CFRelease(point_value); }
                if !size_value.is_null() { CFRelease(size_value); }
                CFRelease(position);
                CFRelease(size);
                return false;
            }
            let moved = AXUIElementSetAttributeValue(window, position, point_value) == 0
                && AXUIElementSetAttributeValue(window, size, size_value) == 0;
            CFRelease(point_value);
            CFRelease(size_value);
            CFRelease(position);
            CFRelease(size);
            moved
        }
    }

    fn snap_window_of(mtm: objc2::MainThreadMarker, pid: i32, action: SnapAction) -> SnapOutcome {
        // SAFETY: the pid belongs to a running application, so the application
        // element is well-formed; every value obtained below is released here.
        unsafe {
            let application = AXUIElementCreateApplication(pid);
            if application.is_null() {
                return SnapOutcome::Refused;
            }
            let mut window: *const std::ffi::c_void = std::ptr::null();
            let focused_attribute = cf_string(b"AXFocusedWindow\0");
            if focused_attribute.is_null() {
                CFRelease(application);
                return SnapOutcome::Refused;
            }
            let focused = AXUIElementCopyAttributeValue(
                application,
                focused_attribute,
                &mut window,
            );
            CFRelease(focused_attribute);
            if focused != 0 || window.is_null() {
                CFRelease(application);
                return SnapOutcome::Refused;
            }
            let (position, size) = match (read_point(window), read_size(window)) {
                (Some(position), Some(size)) => (position, size),
                _ => {
                    CFRelease(window);
                    CFRelease(application);
                    return SnapOutcome::Refused;
                }
            };
            let visibles = screens_ax(mtm);
            let current = Rect {
                x: position.0,
                y: position.1,
                width: size.0,
                height: size.1,
            };
            let Some(visible) = visibles.first().copied() else {
                CFRelease(window);
                CFRelease(application);
                return SnapOutcome::Refused;
            };
            let visible = visibles.get(screen_for(current, &visibles)).copied().unwrap_or(visible);
            let target = target_rect(action, visible, current);
            let moved = write_frame(window, target);
            CFRelease(window);
            CFRelease(application);
            if moved { SnapOutcome::Moved } else { SnapOutcome::Refused }
        }
    }
}

#[cfg(target_os = "macos")]
pub use platform::snap;

#[cfg(not(target_os = "macos"))]
pub fn snap(_action: SnapAction) -> SnapOutcome {
    SnapOutcome::Refused
}
