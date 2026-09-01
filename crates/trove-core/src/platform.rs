//! The contract every platform adapter implements.
//!
//! The core never sees a platform-native object. An adapter reads the system
//! clipboard, turns what it finds into an owned [`ClipboardSnapshot`], and hands
//! that to a [`CaptureSink`]. Everything downstream — hashing, storage,
//! indexing — therefore runs on plain data and stays testable on a machine with
//! no clipboard at all.

use std::sync::Arc;

use serde::Serialize;
use thiserror::Error;

use crate::model::{ContentKind, RepresentationInput, SourceConfidence};

/// How long a write receipt may swallow an echo of our own write.
///
/// A platform normally reports our write as the very next change, and the
/// receipt cancels itself then. If that report never arrives — the write failed
/// silently, another process wrote first — the receipt must stop mattering,
/// otherwise the next genuine copy would vanish from the user's history.
pub const SUPPRESSION_WINDOW_MS: i64 = 2_000;

/// What a platform can actually do, as opposed to what the application would
/// like it to do.
///
/// Every field defaults to false. An adapter opts in to what it has proven it
/// supports, so a new platform starts honest rather than starting optimistic.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClipboardCapabilities {
    /// The adapter reports changes on its own, without the window being open.
    pub continuous_monitoring: bool,
    /// More than plain text survives a round trip.
    pub rich_formats: bool,
    /// The adapter can name the application a copy came from.
    pub source_application: bool,
    /// The platform marks items that must never be recorded.
    pub transient_markers: bool,
    /// The adapter can paste into the previously focused window.
    pub automatic_paste: bool,
    /// Stable reason code shown when the platform cannot do what is expected.
    /// Never a sentence, never a path: the interface maps the code to copy.
    pub degraded_reason: Option<String>,
}

impl ClipboardCapabilities {
    /// True when the platform cannot deliver the full experience and said why.
    pub fn is_degraded(&self) -> bool {
        self.degraded_reason.is_some()
    }
}

/// One reading of the system clipboard, owned end to end.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ClipboardSnapshot {
    pub kind: ContentKind,
    pub primary_mime: String,
    pub representations: Vec<RepresentationInput>,
    pub source_app_id: Option<String>,
    pub source_app_name: Option<String>,
    pub source_confidence: SourceConfidence,
    pub observed_at_ms: i64,
}

/// Receives snapshots from whichever thread the adapter runs on.
///
/// Implementations must not block: an adapter often calls this from a platform
/// callback or the main thread, where blocking would stall the whole interface.
pub trait CaptureSink: Send + Sync {
    fn capture(&self, snapshot: ClipboardSnapshot);
}

/// Proof that a particular clipboard change was caused by this process.
///
/// Suppression is bounded on both axes: it matches exactly one change token and
/// expires after [`SUPPRESSION_WINDOW_MS`]. Guessing by content instead would
/// silently drop a genuine copy of text the user had just pasted from here.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WriteReceipt {
    /// The platform's own identifier for the change we caused.
    pub change_token: u64,
    /// When the write happened, on the same clock the adapter reads.
    pub written_at_ms: i64,
}

impl WriteReceipt {
    pub fn new(change_token: u64, written_at_ms: i64) -> Self {
        Self {
            change_token,
            written_at_ms,
        }
    }

    /// True when this receipt accounts for the given change.
    pub fn matches(&self, change_token: u64, now_ms: i64) -> bool {
        self.change_token == change_token && !self.is_expired(now_ms)
    }

    pub fn is_expired(&self, now_ms: i64) -> bool {
        now_ms.saturating_sub(self.written_at_ms) > SUPPRESSION_WINDOW_MS
    }
}

/// The window that had focus before the palette appeared.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PasteTarget {
    pub process_id: i32,
    pub bundle_id: Option<String>,
}

/// What actually happened when the user asked to paste.
///
/// Automatic paste is best effort everywhere. When it cannot happen the item is
/// still on the clipboard, and the reason is explicit rather than silent.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PasteOutcome {
    /// The keystroke was delivered to the target window.
    Pasted,
    /// Copied only: the platform has not granted the required permission.
    CopiedOnlyPermissionRequired,
    /// Copied only: the window that had focus is gone.
    CopiedOnlyTargetLost,
    /// Copied only: this platform cannot synthesize a paste at all.
    CopiedOnlyPlatformLimit,
}

#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum PlatformError {
    #[error("clipboard_unavailable")]
    Unavailable,
    #[error("clipboard_locked")]
    Locked,
    #[error("clipboard_read_failed")]
    ReadFailed,
    #[error("clipboard_write_failed")]
    WriteFailed,
    #[error("{0}")]
    Degraded(&'static str),
}

/// One clipboard adapter per platform.
///
/// `start` and `stop` bracket monitoring; an adapter that is dropped while
/// running must stop on its own, because leaving a platform listener registered
/// after shutdown leaks a callback into a freed process.
pub trait ClipboardAdapter: Send {
    fn capabilities(&self) -> ClipboardCapabilities;

    fn start(&mut self, sink: Arc<dyn CaptureSink>) -> Result<(), PlatformError>;

    fn stop(&mut self);

    /// Puts an item back on the clipboard and returns proof it was us.
    fn write(&mut self, snapshot: &ClipboardSnapshot) -> Result<WriteReceipt, PlatformError>;

    /// Best-effort synthetic paste into the previously focused window.
    fn paste(&mut self) -> Result<PasteOutcome, PlatformError>;
}
