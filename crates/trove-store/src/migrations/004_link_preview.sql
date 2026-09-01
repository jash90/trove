-- What a link's page said about itself, remembered so it is asked once.
--
-- Fetching is the only thing this application does over the network, and every
-- request tells a server that someone is looking at their clipboard. Keeping
-- the answer — including the failures — is what stops that happening again on
-- every scroll.
CREATE TABLE link_preview (
  content_id INTEGER PRIMARY KEY REFERENCES content(content_id) ON DELETE CASCADE,
  status TEXT NOT NULL CHECK(status IN ('ok', 'empty', 'failed', 'refused')),
  title TEXT CHECK(
    title IS NULL OR (typeof(title) = 'text' AND length(CAST(title AS BLOB)) <= 1024)
  ),
  icon_relpath TEXT,
  icon_mime TEXT CHECK(
    icon_mime IS NULL OR (typeof(icon_mime) = 'text' AND length(CAST(icon_mime AS BLOB)) <= 128)
  ),
  fetched_at_ms INTEGER NOT NULL,
  CHECK(
    icon_relpath IS NULL OR (
      typeof(icon_relpath) = 'text'
        AND length(CAST(icon_relpath AS BLOB)) = 67
        AND substr(icon_relpath, 3, 1) = '/'
        AND substr(icon_relpath, 1, 2) NOT GLOB '*[^0-9a-f]*'
        AND substr(icon_relpath, 4, 64) NOT GLOB '*[^0-9a-f]*'
    )
  ),
  -- An icon without a type could not be rendered, and a type without an icon
  -- describes nothing.
  CHECK((icon_relpath IS NULL) = (icon_mime IS NULL))
);
