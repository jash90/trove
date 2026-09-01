//! What a Linux session can and cannot do with the clipboard.
//!
//! Linux is two different systems wearing one name. Under X11 any client may
//! read the selection at any time, so a history is straightforward. Under
//! Wayland it deliberately cannot: only the focused surface may read, which is
//! exactly the protection a clipboard manager needs to work around — and the
//! way around it, the data-control protocol, is optional and often absent.
//!
//! So this crate's job is to say honestly what the session in front of it
//! supports, rather than to claim a history it cannot deliver.

use trove_core::ClipboardCapabilities;

/// The display server behind the session.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LinuxSession {
    X11,
    /// Wayland with a compositor that implements a data-control protocol.
    WaylandWithDataControl,
    /// Wayland without one. No background reads are possible at all.
    WaylandWithoutDataControl,
    /// Neither could be identified — a bare TTY, or an unusual setup.
    Unknown,
}

/// The environment variables that decide which session this is.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SessionEnv {
    pub session_type: Option<String>,
    pub wayland_display: Option<String>,
    pub x11_display: Option<String>,
    /// Whether a data-control protocol was found on the compositor.
    pub data_control_available: bool,
}

impl SessionEnv {
    pub fn x11() -> Self {
        Self {
            session_type: Some("x11".to_owned()),
            x11_display: Some(":0".to_owned()),
            ..Self::default()
        }
    }

    pub fn wayland(data_control_available: bool) -> Self {
        Self {
            session_type: Some("wayland".to_owned()),
            wayland_display: Some("wayland-0".to_owned()),
            data_control_available,
            ..Self::default()
        }
    }

    /// Reads the session out of the process environment.
    ///
    /// `data_control_available` is left false: proving a protocol exists means
    /// talking to the compositor, and claiming it without checking is the one
    /// mistake that turns a degraded mode into a silent failure.
    pub fn from_process() -> Self {
        Self {
            session_type: std::env::var("XDG_SESSION_TYPE").ok(),
            wayland_display: std::env::var("WAYLAND_DISPLAY").ok(),
            x11_display: std::env::var("DISPLAY").ok(),
            data_control_available: false,
        }
    }
}

/// Decides which session is in front of us.
///
/// `XDG_SESSION_TYPE` is authoritative when set; otherwise the presence of a
/// display socket decides, with Wayland taking precedence because XWayland
/// leaves `DISPLAY` set inside a Wayland session and would otherwise make it
/// look like X11.
pub fn detect_session(env: &SessionEnv) -> LinuxSession {
    let declared = env
        .session_type
        .as_deref()
        .map(|value| value.trim().to_ascii_lowercase());
    let wayland = match declared.as_deref() {
        Some("wayland") => true,
        Some("x11") => false,
        _ => env.wayland_display.is_some(),
    };
    if wayland {
        return if env.data_control_available {
            LinuxSession::WaylandWithDataControl
        } else {
            LinuxSession::WaylandWithoutDataControl
        };
    }
    if declared.as_deref() == Some("x11") || env.x11_display.is_some() {
        return LinuxSession::X11;
    }
    LinuxSession::Unknown
}

/// What the session actually supports.
pub fn detect_capabilities(env: &SessionEnv) -> ClipboardCapabilities {
    match detect_session(env) {
        LinuxSession::X11 => ClipboardCapabilities {
            continuous_monitoring: true,
            rich_formats: true,
            source_application: false,
            transient_markers: false,
            // X11 has no equivalent of the macOS Accessibility grant, but
            // synthesizing a paste needs XTEST, which is not always present.
            automatic_paste: false,
            degraded_reason: Some("x11_synthetic_paste_unavailable".to_owned()),
        },
        LinuxSession::WaylandWithDataControl => ClipboardCapabilities {
            continuous_monitoring: true,
            rich_formats: true,
            source_application: false,
            transient_markers: false,
            automatic_paste: false,
            degraded_reason: Some("wayland_synthetic_paste_unavailable".to_owned()),
        },
        LinuxSession::WaylandWithoutDataControl => ClipboardCapabilities {
            // Nothing may read the clipboard from the background. Read and
            // write on demand still work, so the application is usable — it
            // just cannot build a history on its own.
            degraded_reason: Some("wayland_data_control_unavailable".to_owned()),
            ..ClipboardCapabilities::default()
        },
        LinuxSession::Unknown => ClipboardCapabilities {
            degraded_reason: Some("no_display_server".to_owned()),
            ..ClipboardCapabilities::default()
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_x11_session_can_keep_a_history() {
        let capabilities = detect_capabilities(&SessionEnv::x11());

        assert_eq!(detect_session(&SessionEnv::x11()), LinuxSession::X11);
        assert!(capabilities.continuous_monitoring);
        assert!(!capabilities.automatic_paste);
    }

    #[test]
    fn wayland_without_data_control_says_so_instead_of_pretending() {
        let env = SessionEnv::wayland(false);
        let capabilities = detect_capabilities(&env);

        assert_eq!(
            detect_session(&env),
            LinuxSession::WaylandWithoutDataControl
        );
        assert!(!capabilities.continuous_monitoring);
        assert!(!capabilities.automatic_paste);
        assert_eq!(
            capabilities.degraded_reason.as_deref(),
            Some("wayland_data_control_unavailable")
        );
    }

    #[test]
    fn wayland_with_data_control_can_keep_a_history() {
        let env = SessionEnv::wayland(true);

        assert_eq!(detect_session(&env), LinuxSession::WaylandWithDataControl);
        assert!(detect_capabilities(&env).continuous_monitoring);
    }

    #[test]
    fn xwayland_does_not_make_a_wayland_session_look_like_x11() {
        // XWayland leaves DISPLAY set. Reading it as X11 would promise a
        // history the session cannot deliver.
        let env = SessionEnv {
            session_type: Some("wayland".to_owned()),
            wayland_display: Some("wayland-0".to_owned()),
            x11_display: Some(":0".to_owned()),
            data_control_available: false,
        };

        assert_eq!(
            detect_session(&env),
            LinuxSession::WaylandWithoutDataControl
        );
    }

    #[test]
    fn a_wayland_socket_without_a_declared_type_still_reads_as_wayland() {
        let env = SessionEnv {
            wayland_display: Some("wayland-0".to_owned()),
            x11_display: Some(":0".to_owned()),
            ..SessionEnv::default()
        };

        assert_eq!(
            detect_session(&env),
            LinuxSession::WaylandWithoutDataControl
        );
    }

    #[test]
    fn no_display_server_at_all_is_named_rather_than_guessed() {
        let capabilities = detect_capabilities(&SessionEnv::default());

        assert_eq!(
            detect_session(&SessionEnv::default()),
            LinuxSession::Unknown
        );
        assert!(capabilities.is_degraded());
        assert_eq!(
            capabilities.degraded_reason.as_deref(),
            Some("no_display_server")
        );
    }
}
