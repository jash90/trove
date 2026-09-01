//! The Windows clipboard adapter.
//!
//! Windows is the one platform that reports clipboard changes instead of making
//! you look: a hidden window registered with `AddClipboardFormatListener`
//! receives `WM_CLIPBOARDUPDATE`. No polling, no change counter.
//!
//! What it does not have is a way to enforce privacy markers. Instead there are
//! three advisory formats an application sets to say "leave this out of
//! clipboard history", and Windows' own history honours them. So does this one.

use trove_core::ClipboardCapabilities;

/// Formats that ask every clipboard history to skip an item.
///
/// These are conventions, not permissions — nothing stops a program reading the
/// data anyway. A password manager that sets them is trusting us, which is
/// exactly why they are checked before anything is read.
pub const EXCLUDE_FROM_MONITORING: &str = "ExcludeClipboardContentFromMonitorProcessing";
pub const CAN_INCLUDE_IN_HISTORY: &str = "CanIncludeInClipboardHistory";
pub const CAN_UPLOAD_TO_CLOUD: &str = "CanUploadToCloudClipboard";

/// Whether an item may be recorded, decided from the formats alone.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExclusionPolicy {
    Accept,
    /// The item asked to be left out of any monitoring.
    RejectExcluded,
    /// The item allowed history explicitly set to off.
    RejectHistoryDenied,
}

impl ExclusionPolicy {
    pub fn is_rejected(self) -> bool {
        !matches!(self, Self::Accept)
    }

    pub fn reason(self) -> Option<&'static str> {
        match self {
            Self::Accept => None,
            Self::RejectExcluded => Some("excluded_from_monitoring"),
            Self::RejectHistoryDenied => Some("history_denied"),
        }
    }
}

/// One advertised clipboard format and the value it carries, when it has one.
///
/// `CanIncludeInClipboardHistory` is a DWORD: present and zero means no. Simply
/// being on the clipboard is not a refusal, which is why the value matters.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AdvertisedFormat {
    pub name: String,
    pub value: Option<u32>,
}

impl AdvertisedFormat {
    pub fn flag(name: &str, value: u32) -> Self {
        Self {
            name: name.to_owned(),
            value: Some(value),
        }
    }

    pub fn marker(name: &str) -> Self {
        Self {
            name: name.to_owned(),
            value: None,
        }
    }
}

/// Decides whether an item may be recorded.
pub fn classify_formats(formats: &[AdvertisedFormat]) -> ExclusionPolicy {
    let find = |wanted: &str| formats.iter().find(|format| format.name == wanted);
    if find(EXCLUDE_FROM_MONITORING).is_some() {
        return ExclusionPolicy::RejectExcluded;
    }
    if let Some(format) = find(CAN_INCLUDE_IN_HISTORY)
        && format.value == Some(0)
    {
        return ExclusionPolicy::RejectHistoryDenied;
    }
    ExclusionPolicy::Accept
}

/// What Windows supports.
pub fn capabilities() -> ClipboardCapabilities {
    ClipboardCapabilities {
        // The system tells us when the clipboard changed, so there is nothing
        // to poll.
        continuous_monitoring: true,
        rich_formats: true,
        // GetClipboardOwner names a window, which usually maps to the
        // application that copied — a guess, not a declaration.
        source_application: true,
        transient_markers: true,
        // SendInput is blocked from reaching a process running at a higher
        // integrity level, so a paste cannot be promised in general.
        automatic_paste: false,
        degraded_reason: Some("windows_synthetic_paste_may_be_blocked".to_owned()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_ordinary_item_is_accepted() {
        let formats = vec![AdvertisedFormat::marker("CF_UNICODETEXT")];

        assert_eq!(classify_formats(&formats), ExclusionPolicy::Accept);
        assert_eq!(ExclusionPolicy::Accept.reason(), None);
    }

    #[test]
    fn an_item_asking_to_be_excluded_is_never_recorded() {
        let formats = vec![
            AdvertisedFormat::marker("CF_UNICODETEXT"),
            AdvertisedFormat::marker(EXCLUDE_FROM_MONITORING),
        ];

        assert_eq!(classify_formats(&formats), ExclusionPolicy::RejectExcluded);
        assert!(classify_formats(&formats).is_rejected());
    }

    #[test]
    fn history_denied_means_zero_not_merely_present() {
        // The format carries a value. Treating its presence as a refusal would
        // drop every item from an application that sets it to one.
        let denied = vec![AdvertisedFormat::flag(CAN_INCLUDE_IN_HISTORY, 0)];
        let allowed = vec![AdvertisedFormat::flag(CAN_INCLUDE_IN_HISTORY, 1)];

        assert_eq!(
            classify_formats(&denied),
            ExclusionPolicy::RejectHistoryDenied
        );
        assert_eq!(classify_formats(&allowed), ExclusionPolicy::Accept);
    }

    #[test]
    fn exclusion_outranks_a_permissive_history_flag() {
        let formats = vec![
            AdvertisedFormat::flag(CAN_INCLUDE_IN_HISTORY, 1),
            AdvertisedFormat::marker(EXCLUDE_FROM_MONITORING),
        ];

        assert_eq!(classify_formats(&formats), ExclusionPolicy::RejectExcluded);
    }

    #[test]
    fn windows_admits_it_cannot_promise_a_paste() {
        let capabilities = capabilities();

        assert!(capabilities.continuous_monitoring);
        assert!(!capabilities.automatic_paste);
        assert!(capabilities.is_degraded());
    }

    #[test]
    fn the_cloud_upload_flag_is_not_a_reason_to_drop_an_item() {
        // It governs syncing between machines, which this application never
        // does. Refusing on it would drop items nobody asked us to drop.
        let formats = vec![AdvertisedFormat::flag(CAN_UPLOAD_TO_CLOUD, 0)];

        assert_eq!(classify_formats(&formats), ExclusionPolicy::Accept);
    }
}
