//! The platform contract, exercised through a fake adapter.
//!
//! Every value crossing this boundary is owned and serialized: no
//! platform-native object may escape an adapter, because the rest of the core
//! must compile and be testable on a machine that has no clipboard at all.

use std::sync::{Arc, Mutex};

use clipboard_core::{
    CaptureSink, ClipboardAdapter, ClipboardCapabilities, ClipboardSnapshot, ContentKind,
    PasteOutcome, PlatformError, RepresentationInput, SUPPRESSION_WINDOW_MS, SourceConfidence,
    WriteReceipt,
};

#[derive(Clone, Default)]
struct RecordingSink {
    captured: Arc<Mutex<Vec<String>>>,
}

impl RecordingSink {
    fn payloads(&self) -> Vec<String> {
        self.captured.lock().unwrap().clone()
    }
}

impl CaptureSink for RecordingSink {
    fn capture(&self, snapshot: ClipboardSnapshot) {
        let text = snapshot
            .representations
            .first()
            .and_then(|representation| representation.bytes.as_deref())
            .map(|bytes| String::from_utf8_lossy(bytes).into_owned())
            .unwrap_or_default();
        self.captured.lock().unwrap().push(text);
    }
}

fn text_snapshot(value: &str) -> ClipboardSnapshot {
    ClipboardSnapshot {
        kind: ContentKind::Text,
        primary_mime: "text/plain".to_owned(),
        representations: vec![RepresentationInput {
            format_id: "text/plain".to_owned(),
            bytes: Some(value.as_bytes().to_vec()),
            missing_ref: None,
        }],
        source_app_id: None,
        source_app_name: None,
        source_confidence: SourceConfidence::Unknown,
        observed_at_ms: 1_725_000_000_000,
    }
}

/// Stands in for a real platform. Change tokens are handed out in order, the
/// same way a change counter increases, and the clock is driven by the test.
struct FakeAdapter {
    sink: Option<Arc<dyn CaptureSink>>,
    next_token: u64,
    now_ms: i64,
    suppression: Option<WriteReceipt>,
    written: Vec<ClipboardSnapshot>,
}

impl FakeAdapter {
    fn new() -> Self {
        Self {
            sink: None,
            next_token: 1,
            now_ms: 0,
            suppression: None,
            written: Vec::new(),
        }
    }

    fn advance(&mut self, milliseconds: i64) {
        self.now_ms += milliseconds;
    }

    /// A change this process caused. The receipt must swallow it once.
    fn emit_change(&mut self, token: u64, snapshot: ClipboardSnapshot) {
        if let Some(receipt) = self.suppression {
            if receipt.matches(token, self.now_ms) {
                self.suppression = None;
                return;
            }
            if receipt.is_expired(self.now_ms) {
                self.suppression = None;
            }
        }
        if let Some(sink) = self.sink.as_ref() {
            sink.capture(snapshot);
        }
    }

    fn emit_external(&mut self, snapshot: ClipboardSnapshot) {
        let token = self.next_token;
        self.next_token += 1;
        self.emit_change(token, snapshot);
    }
}

impl ClipboardAdapter for FakeAdapter {
    fn capabilities(&self) -> ClipboardCapabilities {
        ClipboardCapabilities {
            continuous_monitoring: true,
            ..ClipboardCapabilities::default()
        }
    }

    fn start(&mut self, sink: Arc<dyn CaptureSink>) -> Result<(), PlatformError> {
        self.sink = Some(sink);
        Ok(())
    }

    fn stop(&mut self) {
        self.sink = None;
    }

    fn write(&mut self, snapshot: &ClipboardSnapshot) -> Result<WriteReceipt, PlatformError> {
        let token = self.next_token;
        self.next_token += 1;
        self.written.push(snapshot.clone());
        let receipt = WriteReceipt::new(token, self.now_ms);
        self.suppression = Some(receipt);
        Ok(receipt)
    }

    fn paste(&mut self) -> Result<PasteOutcome, PlatformError> {
        Ok(PasteOutcome::CopiedOnlyPlatformLimit)
    }
}

#[test]
fn a_write_receipt_suppresses_exactly_one_echo_and_nothing_else() {
    let sink = RecordingSink::default();
    let mut adapter = FakeAdapter::new();
    adapter.start(Arc::new(sink.clone())).unwrap();

    let receipt = adapter.write(&text_snapshot("nasze")).unwrap();
    adapter.emit_change(receipt.change_token, text_snapshot("nasze"));
    adapter.emit_external(text_snapshot("cudze"));

    assert_eq!(sink.payloads(), vec!["cudze".to_owned()]);
}

#[test]
fn a_receipt_for_a_different_change_suppresses_nothing() {
    let sink = RecordingSink::default();
    let mut adapter = FakeAdapter::new();
    adapter.start(Arc::new(sink.clone())).unwrap();

    let receipt = adapter.write(&text_snapshot("nasze")).unwrap();
    adapter.emit_change(receipt.change_token + 1, text_snapshot("cudze"));

    assert_eq!(sink.payloads(), vec!["cudze".to_owned()]);
}

#[test]
fn suppression_expires_so_a_lost_echo_cannot_swallow_a_later_copy() {
    let sink = RecordingSink::default();
    let mut adapter = FakeAdapter::new();
    adapter.start(Arc::new(sink.clone())).unwrap();

    let receipt = adapter.write(&text_snapshot("nasze")).unwrap();
    adapter.advance(SUPPRESSION_WINDOW_MS + 1);
    adapter.emit_change(receipt.change_token, text_snapshot("cudze"));

    assert_eq!(sink.payloads(), vec!["cudze".to_owned()]);
}

#[test]
fn stopping_the_monitor_ends_capture() {
    let sink = RecordingSink::default();
    let mut adapter = FakeAdapter::new();
    adapter.start(Arc::new(sink.clone())).unwrap();

    adapter.emit_external(text_snapshot("przed"));
    adapter.stop();
    adapter.emit_external(text_snapshot("po"));

    assert_eq!(sink.payloads(), vec!["przed".to_owned()]);
}

#[test]
fn a_degraded_platform_states_its_reason_instead_of_claiming_support() {
    let capabilities = ClipboardCapabilities {
        degraded_reason: Some("wayland_data_control_unavailable".to_owned()),
        ..ClipboardCapabilities::default()
    };

    assert!(!capabilities.continuous_monitoring);
    assert!(!capabilities.automatic_paste);
    assert!(capabilities.is_degraded());
}

#[test]
fn full_support_is_not_degraded() {
    let capabilities = ClipboardCapabilities {
        continuous_monitoring: true,
        rich_formats: true,
        source_application: true,
        transient_markers: true,
        automatic_paste: true,
        degraded_reason: None,
    };

    assert!(!capabilities.is_degraded());
}
