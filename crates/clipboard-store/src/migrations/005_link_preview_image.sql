-- The picture a page nominates for itself, kept beside its title.
--
-- Separate from the icon: an icon is a mark a few pixels across, this is the
-- card the page wants shown when it is linked to. Stored downscaled, so a
-- history full of links does not become a picture archive.
ALTER TABLE link_preview ADD COLUMN image_relpath TEXT;
ALTER TABLE link_preview ADD COLUMN image_mime TEXT;
