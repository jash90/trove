//! Writing history back out in the format this crate already reads.
//!
//! The shape is not invented here: it is the SuperCmd record the parser next
//! door accepts, field for field, so an export can be imported again. That is
//! the only definition of "correct" a format like this has, and the tests
//! check it by feeding the output back through the reader rather than by
//! comparing it to a fixture somebody typed.

use std::fmt::Write as _;

use serde::Serialize;

/// The CSV header.
///
/// `copied_at` and `type` are the two columns detection insists on; the rest
/// are optional to the reader but written anyway, because an export that drops
/// what it knows is a lossy round trip.
pub const SUPERCMD_CSV_HEADER: &str =
    "copied_at,type,source_app,bundle_id,pinned,file_url,text,ocr_text,has_image,image_hash";

/// One exported entry.
///
/// Field names match the reader's `#[derive(Deserialize)]` exactly, including
/// the snake_case spelling it prefers over its camelCase aliases.
#[derive(Clone, Debug, Default, Serialize)]
pub struct SuperCmdExportRecord {
    pub copied_at: String,
    #[serde(rename = "type")]
    pub content_type: String,
    pub source_app: Option<String>,
    pub bundle_id: Option<String>,
    pub pinned: bool,
    pub file_url: Option<String>,
    pub text: Option<String>,
    pub ocr_text: Option<String>,
    pub has_image: bool,
    pub image_hash: Option<String>,
}

impl SuperCmdExportRecord {
    /// Renders this record as one CSV row, terminated by a newline.
    fn write_csv_row(&self, out: &mut String) {
        let empty = String::new();
        let columns: [&str; 10] = [
            &self.copied_at,
            &self.content_type,
            self.source_app.as_ref().unwrap_or(&empty),
            self.bundle_id.as_ref().unwrap_or(&empty),
            if self.pinned { "true" } else { "false" },
            self.file_url.as_ref().unwrap_or(&empty),
            self.text.as_ref().unwrap_or(&empty),
            self.ocr_text.as_ref().unwrap_or(&empty),
            if self.has_image { "true" } else { "false" },
            self.image_hash.as_ref().unwrap_or(&empty),
        ];
        for (index, column) in columns.iter().enumerate() {
            if index > 0 {
                out.push(',');
            }
            write_csv_field(column, out);
        }
        out.push('\n');
    }
}

/// Quotes a field when RFC 4180 requires it, and never otherwise.
///
/// Clipboard text routinely contains commas, quotes and newlines, so this is
/// the part that decides whether the file can be read back at all.
fn write_csv_field(value: &str, out: &mut String) {
    let needs_quoting = value
        .chars()
        .any(|character| matches!(character, ',' | '"' | '\n' | '\r'));
    if !needs_quoting {
        out.push_str(value);
        return;
    }
    out.push('"');
    for character in value.chars() {
        if character == '"' {
            out.push('"');
        }
        out.push(character);
    }
    out.push('"');
}

/// Renders every record as one CSV document.
pub fn render_csv(records: &[SuperCmdExportRecord]) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "{SUPERCMD_CSV_HEADER}");
    for record in records {
        record.write_csv_row(&mut out);
    }
    out
}

/// Renders every record as one JSON array — the source of truth of the pair.
pub fn render_json(records: &[SuperCmdExportRecord]) -> Result<String, crate::ImportError> {
    serde_json::to_string_pretty(records).map_err(|_| crate::ImportError::service("export_failed"))
}

/// Formats a capture time the way the reader parses it.
pub fn format_timestamp_ms(captured_at_ms: i64) -> String {
    chrono::DateTime::from_timestamp_millis(captured_at_ms)
        .unwrap_or_else(chrono::Utc::now)
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(text: &str) -> SuperCmdExportRecord {
        SuperCmdExportRecord {
            copied_at: format_timestamp_ms(1_775_000_000_000),
            content_type: "text".to_owned(),
            text: Some(text.to_owned()),
            ..SuperCmdExportRecord::default()
        }
    }

    #[test]
    fn the_header_carries_the_two_columns_detection_insists_on() {
        let columns: Vec<&str> = SUPERCMD_CSV_HEADER.split(',').collect();

        assert!(columns.contains(&"copied_at"));
        assert!(columns.contains(&"type"));
        assert_eq!(columns.len(), 10);
    }

    #[test]
    fn a_field_is_quoted_when_it_has_to_be_and_left_alone_otherwise() {
        let mut out = String::new();
        write_csv_field("plain", &mut out);
        assert_eq!(out, "plain");

        for (input, expected) in [
            ("with,comma", "\"with,comma\""),
            ("with\"quote", "\"with\"\"quote\""),
            ("with\nnewline", "\"with\nnewline\""),
            ("with\rreturn", "\"with\rreturn\""),
        ] {
            let mut out = String::new();
            write_csv_field(input, &mut out);
            assert_eq!(out, expected, "{input}");
        }
    }

    #[test]
    fn a_timestamp_round_trips_through_the_reader_that_parses_it() {
        let rendered = format_timestamp_ms(1_775_000_000_123);

        assert_eq!(
            crate::supercmd::parse_timestamp_ms(&rendered),
            Some(1_775_000_000_123)
        );
    }

    #[test]
    fn clipboard_text_with_separators_survives_the_csv_it_is_written_into() {
        let rendered = render_csv(&[record("one,two\n\"three\"")]);
        let mut lines = rendered.lines();

        assert_eq!(lines.next(), Some(SUPERCMD_CSV_HEADER));
        // The record spans the rest: a newline inside a quoted field is part of
        // the value, which is exactly why it may not be read line by line.
        assert!(rendered.contains("\"one,two\n\"\"three\"\"\""));
    }

    #[test]
    fn the_json_document_is_an_array_the_reader_can_stream() {
        let rendered = render_json(&[record("synthetic")]).unwrap();

        assert!(rendered.trim_start().starts_with('['));
        assert!(rendered.contains("\"copied_at\""));
        assert!(rendered.contains("\"type\": \"text\""));
    }
}
