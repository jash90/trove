import { FolderSearch } from 'lucide-react';

import { fileBasename } from '../lib/format';

interface SourceLocationProps {
  path: string;
  /** Whether the file is still there. Imports never keep an entry that is not,
   *  but a file can be moved after the fact; the action simply goes away. */
  exists: boolean;
  onReveal?: () => void;
}

/**
 * A file or image entry keeps where it came from, never the bytes: the importer
 * refuses to read anything outside the selected export. The list shows the name
 * alone, and the full location appears only here, on the selected entry.
 */
export const SourceLocation = ({
  path,
  exists,
  onReveal,
}: SourceLocationProps): React.JSX.Element => (
  <section className="source-location" aria-labelledby="source-location-title">
    <span className="preview-label">Source location</span>
    <h3 id="source-location-title">{fileBasename(path)}</h3>
    <p className="source-location__path">{path}</p>
    {exists ? (
      <button type="button" className="source-location__reveal" onClick={onReveal}>
        <FolderSearch size={15} aria-hidden="true" />
        Show in Finder
      </button>
    ) : null}
  </section>
);
