-- A compact prefix lookup for fragments too short for the trigram index.
-- Store at most two characters per document, not a second copy of its text.
CREATE INDEX idx_search_doc_prefix ON search_doc(substr(normalized_text, 1, 2));
