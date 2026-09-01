-- Blob reclamation asks, for each file in the store, whether any row still
-- refers to it. Without these indexes each of those questions is a full scan of
-- every payload row, so one bounded pass costs a multiple of the whole history
-- and gets slower every day the history grows. The pass is bounded in files
-- examined; these make it bounded in work too.
CREATE INDEX idx_raw_payload_blob_relpath
  ON raw_payload(blob_relpath) WHERE blob_relpath IS NOT NULL;

CREATE INDEX idx_artifact_blob_relpath
  ON artifact(blob_relpath);
