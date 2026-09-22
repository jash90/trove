-- The search index, rebuilt on trigrams instead of words.
--
-- The word index could only find a clip from the start of one of its words:
-- "cmd" did not find "supercmd", and a fragment from the middle of a URL, a
-- path or an identifier found nothing at all. Trigrams match a fragment
-- wherever it sits. The text they index is already lowercased and stripped
-- of diacritics on write, so case and accents do not matter either.
--
-- Same name, same external content, same triggers: they live on search_doc
-- and refer to this table by name, so they survive the drop. The rebuild
-- re-reads every search document once; that is the whole cost of the change.
DROP TABLE search_fts;

CREATE VIRTUAL TABLE search_fts USING fts5(
  normalized_text,
  content = 'search_doc',
  content_rowid = 'content_id',
  tokenize = 'trigram'
);

INSERT INTO search_fts(search_fts) VALUES('rebuild');
