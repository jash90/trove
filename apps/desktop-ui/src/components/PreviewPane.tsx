import { Eye, X } from 'lucide-react';
import type { ReactNode } from 'react';

import type {
  HistoryItem,
  LinkPreview as LinkPreviewContract,
  Preview,
} from '../lib/contracts';
import { formatByteSize, formatCapturedAt, KIND_LABELS } from '../lib/format';
import { LinkPreviewCard } from './LinkPreviewCard';
import { ImagePreview, type ThumbnailStatus } from './ImagePreview';
import { SourceLocation } from './SourceLocation';
import { TextPreview } from './TextPreview';

export type PreviewStatus = 'idle' | 'loading' | 'ready' | 'error';

/// The store already caps occurrences per content; slicing again means a
/// malformed page cannot turn the preview into an unbounded list.
const MAX_OCCURRENCE_STAMPS = 5;

interface PreviewPaneProps {
  preview: Preview | null;
  status?: PreviewStatus;
  thumbnailUrl: string | null;
  thumbnailStatus: ThumbnailStatus;
  /** What the selected link points at, when the entry is a link. */
  linkPreview?: LinkPreviewContract | null;
  /** The selected list row, when one is selected — the group it fronts. */
  selectedItem?: HistoryItem | null;
  actions?: ReactNode;
  onClose?: () => void;
  onRevealSource?: () => void;
}

export const PreviewPane = ({
  preview,
  status = preview ? 'ready' : 'idle',
  thumbnailUrl,
  thumbnailStatus,
  linkPreview = null,
  selectedItem = null,
  actions,
  onClose,
  onRevealSource,
}: PreviewPaneProps): React.JSX.Element => {
  return (
    <aside className="preview-pane" aria-label="Selected entry preview">
      <header className="preview-pane__header">
        <h2>{preview ? KIND_LABELS[preview.kind] : 'Select an entry'}</h2>
        {onClose ? (
          <button
            type="button"
            className="preview-pane__close"
            aria-label="Close preview"
            onClick={onClose}
          >
            <X size={16} aria-hidden="true" />
          </button>
        ) : null}
      </header>

      <div className="preview-pane__body">
        {status === 'loading' ? (
          <div className="preview-placeholder" role="status">
            <Eye size={24} aria-hidden="true" />
            <span>Loading preview…</span>
          </div>
        ) : null}
        {status === 'error' ? (
          <div className="preview-placeholder" role="alert">
            <Eye size={24} aria-hidden="true" />
            <strong>The preview could not be loaded</strong>
            <span>Select the entry again, or try later.</span>
          </div>
        ) : null}
        {status === 'idle' ? (
          <div className="preview-placeholder">
            <Eye size={24} aria-hidden="true" />
            <span>Choose an entry from the history.</span>
          </div>
        ) : null}
        {status === 'ready' && preview ? (
          <>
            {preview.kind === 'image' ? (
              <ImagePreview
                thumbnailUrl={thumbnailUrl}
                thumbnailStatus={thumbnailStatus}
              />
            ) : preview.kind === 'link' && linkPreview ? (
              <LinkPreviewCard preview={linkPreview} />
            ) : (
              <TextPreview preview={preview} />
            )}
            {preview.sourcePath ? (
              <SourceLocation
                path={preview.sourcePath}
                exists={preview.sourceExists}
                onReveal={onRevealSource}
              />
            ) : null}
            <dl className="preview-metadata">
              <div>
                <dt>Source</dt>
                <dd>{preview.sourceAppName ?? 'Unknown application'}</dd>
              </div>
              <div>
                <dt>Rozmiar</dt>
                <dd>{formatByteSize(preview.byteSize)}</dd>
              </div>
              {selectedItem && selectedItem.occurrences.length > 0 ? (
                <div>
                  <dt>Captured</dt>
                  <dd>
                    <ul
                      className="preview-metadata__occurrences"
                      aria-label={`Captured ${selectedItem.occurrenceCount} times, newest first`}
                    >
                      {selectedItem.occurrences
                        .slice(0, MAX_OCCURRENCE_STAMPS)
                        .map((capturedAtMs, index) => (
                          <li key={`${index}-${capturedAtMs}`}>
                            <time dateTime={new Date(capturedAtMs).toISOString()}>
                              {formatCapturedAt(capturedAtMs)}
                            </time>
                          </li>
                        ))}
                    </ul>
                  </dd>
                </div>
              ) : null}
            </dl>
          </>
        ) : null}
      </div>
      {actions}
    </aside>
  );
};
