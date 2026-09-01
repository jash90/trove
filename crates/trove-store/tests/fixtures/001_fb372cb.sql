CREATE TABLE schema_identity (
  identity TEXT PRIMARY KEY CHECK(identity = 'trove-store'),
  revision INTEGER NOT NULL CHECK(revision = 2)
);

INSERT INTO schema_identity(identity, revision) VALUES ('trove-store', 2);

CREATE TABLE content (
  content_id INTEGER PRIMARY KEY,
  content_hash BLOB NOT NULL UNIQUE CHECK(length(content_hash) = 32),
  kind TEXT NOT NULL,
  primary_mime TEXT NOT NULL,
  byte_size INTEGER NOT NULL CHECK(byte_size >= 0),
  flags INTEGER NOT NULL DEFAULT 0,
  created_at_ms INTEGER NOT NULL
);

CREATE TABLE content_representation (
  representation_id INTEGER PRIMARY KEY,
  content_id INTEGER NOT NULL REFERENCES content(content_id) ON DELETE CASCADE,
  format_id TEXT NOT NULL,
  storage_kind TEXT NOT NULL CHECK(storage_kind IN ('inline', 'inline_zstd', 'cas', 'missing')),
  inline_payload BLOB,
  blob_relpath TEXT,
  missing_ref TEXT,
  original_byte_size INTEGER NOT NULL CHECK(original_byte_size >= 0),
  stored_byte_size INTEGER NOT NULL CHECK(stored_byte_size >= 0),
  CHECK(
    (storage_kind = 'inline'
      AND inline_payload IS NOT NULL AND blob_relpath IS NULL AND missing_ref IS NULL
      AND original_byte_size < 4096 AND stored_byte_size = length(inline_payload))
    OR (storage_kind = 'inline_zstd'
      AND inline_payload IS NOT NULL AND blob_relpath IS NULL AND missing_ref IS NULL
      AND original_byte_size >= 4096 AND original_byte_size <= 262144
      AND stored_byte_size = length(inline_payload))
    OR (storage_kind = 'cas'
      AND inline_payload IS NULL AND blob_relpath IS NOT NULL AND missing_ref IS NULL
      AND stored_byte_size = original_byte_size)
    OR (storage_kind = 'missing'
      AND inline_payload IS NULL AND blob_relpath IS NULL AND missing_ref IS NOT NULL
      AND original_byte_size = 0 AND stored_byte_size = 0)
  ),
  UNIQUE(content_id, format_id)
);

CREATE TABLE history_event (
  event_id INTEGER PRIMARY KEY,
  global_id BLOB NOT NULL UNIQUE CHECK(length(global_id) = 16),
  content_id INTEGER NOT NULL REFERENCES content(content_id),
  captured_at_ms INTEGER NOT NULL,
  source_app_id TEXT,
  source_app_name TEXT,
  source_app_original TEXT,
  source_confidence TEXT NOT NULL DEFAULT 'unknown',
  pinned INTEGER NOT NULL DEFAULT 0 CHECK(pinned IN (0, 1)),
  occurrence_count INTEGER NOT NULL DEFAULT 1 CHECK(occurrence_count > 0),
  paste_count INTEGER NOT NULL DEFAULT 0 CHECK(paste_count >= 0),
  last_pasted_at_ms INTEGER,
  expires_at_ms INTEGER,
  flags INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE search_doc (
  content_id INTEGER PRIMARY KEY REFERENCES content(content_id) ON DELETE CASCADE,
  normalized_text TEXT NOT NULL
);

CREATE TABLE search_derivation (
  content_id INTEGER NOT NULL REFERENCES content(content_id) ON DELETE CASCADE,
  derivation_hash BLOB NOT NULL CHECK(length(derivation_hash) = 32),
  normalized_text TEXT NOT NULL,
  PRIMARY KEY(content_id, derivation_hash)
);

CREATE VIRTUAL TABLE search_fts USING fts5(
  normalized_text,
  content = 'search_doc',
  content_rowid = 'content_id',
  tokenize = 'unicode61 remove_diacritics 2'
);

CREATE TRIGGER search_doc_after_insert AFTER INSERT ON search_doc BEGIN
  INSERT INTO search_fts(rowid, normalized_text)
  VALUES (new.content_id, new.normalized_text);
END;

CREATE TRIGGER search_doc_after_delete AFTER DELETE ON search_doc BEGIN
  INSERT INTO search_fts(search_fts, rowid, normalized_text)
  VALUES ('delete', old.content_id, old.normalized_text);
END;

CREATE TRIGGER search_doc_after_update AFTER UPDATE ON search_doc BEGIN
  INSERT INTO search_fts(search_fts, rowid, normalized_text)
  VALUES ('delete', old.content_id, old.normalized_text);
  INSERT INTO search_fts(rowid, normalized_text)
  VALUES (new.content_id, new.normalized_text);
END;

CREATE TABLE artifact (
  artifact_id INTEGER PRIMARY KEY,
  content_id INTEGER NOT NULL REFERENCES content(content_id) ON DELETE CASCADE,
  artifact_kind TEXT NOT NULL,
  blob_relpath TEXT NOT NULL,
  byte_size INTEGER NOT NULL CHECK(byte_size >= 0),
  created_at_ms INTEGER NOT NULL,
  UNIQUE(content_id, artifact_kind)
);

CREATE TABLE import_run (
  import_run_id INTEGER PRIMARY KEY,
  external_id BLOB NOT NULL UNIQUE CHECK(length(external_id) = 16),
  source_kind TEXT NOT NULL CHECK(source_kind IN ('raycast', 'supercmd')),
  source_fingerprint BLOB NOT NULL CHECK(length(source_fingerprint) = 32),
  status TEXT NOT NULL CHECK(status IN ('running', 'completed', 'failed')),
  worker_generation INTEGER NOT NULL DEFAULT 1 CHECK(worker_generation > 0),
  total_records INTEGER NOT NULL DEFAULT 0 CHECK(total_records >= 0),
  candidate_records INTEGER NOT NULL DEFAULT 0 CHECK(candidate_records >= 0),
  next_candidate_offset INTEGER NOT NULL DEFAULT 0 CHECK(next_candidate_offset >= 0),
  imported_records INTEGER NOT NULL DEFAULT 0 CHECK(imported_records >= 0),
  already_present_records INTEGER NOT NULL DEFAULT 0 CHECK(already_present_records >= 0),
  skipped_records INTEGER NOT NULL DEFAULT 0 CHECK(skipped_records >= 0),
  failed_records INTEGER NOT NULL DEFAULT 0 CHECK(failed_records >= 0),
  started_at_ms INTEGER NOT NULL,
  finished_at_ms INTEGER,
  error_code TEXT,
  CHECK(candidate_records <= total_records),
  CHECK(next_candidate_offset <= candidate_records),
  CHECK(
    imported_records + already_present_records + skipped_records + failed_records
      <= total_records
  ),
  CHECK(
    status != 'completed'
      OR imported_records + already_present_records + skipped_records + failed_records
        = total_records
  )
);

CREATE TABLE import_failure_reason (
  import_run_id INTEGER NOT NULL REFERENCES import_run(import_run_id) ON DELETE CASCADE,
  reason_code TEXT NOT NULL,
  count INTEGER NOT NULL CHECK(count > 0),
  PRIMARY KEY(import_run_id, reason_code)
);

CREATE TABLE import_record (
  import_record_id INTEGER PRIMARY KEY,
  import_run_id INTEGER NOT NULL REFERENCES import_run(import_run_id) ON DELETE RESTRICT,
  source_kind TEXT NOT NULL CHECK(source_kind IN ('raycast', 'supercmd')),
  record_fingerprint BLOB NOT NULL CHECK(length(record_fingerprint) = 32),
  event_id INTEGER REFERENCES history_event(event_id) ON DELETE SET NULL,
  created_at_ms INTEGER NOT NULL,
  UNIQUE(source_kind, record_fingerprint)
);

CREATE INDEX idx_history_event_timeline
  ON history_event(captured_at_ms DESC, event_id DESC);
CREATE INDEX idx_history_event_content
  ON history_event(content_id, captured_at_ms DESC, event_id DESC);
CREATE INDEX idx_history_event_pins
  ON history_event(captured_at_ms DESC, event_id DESC) WHERE pinned = 1;
CREATE INDEX idx_import_run_source_fingerprint
  ON import_run(source_kind, source_fingerprint);
CREATE INDEX idx_import_record_source_fingerprint
  ON import_record(source_kind, record_fingerprint);

INSERT INTO search_fts(search_fts) VALUES('rebuild');
