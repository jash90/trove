import { Eye, X } from 'lucide-react';
import type { ReactNode } from 'react';

import type { Preview } from '../lib/contracts';
import { formatByteSize, KIND_LABELS } from '../lib/format';
import { ImagePreview, type ThumbnailStatus } from './ImagePreview';
import { SourceLocation } from './SourceLocation';
import { TextPreview } from './TextPreview';

export type PreviewStatus = 'idle' | 'loading' | 'ready' | 'error';

interface PreviewPaneProps {
  preview: Preview | null;
  status?: PreviewStatus;
  thumbnailUrl: string | null;
  thumbnailStatus: ThumbnailStatus;
  actions?: ReactNode;
  onClose?: () => void;
  onRevealSource?: () => void;
}

export const PreviewPane = ({
  preview,
  status = preview ? 'ready' : 'idle',
  thumbnailUrl,
  thumbnailStatus,
  actions,
  onClose,
  onRevealSource,
}: PreviewPaneProps): React.JSX.Element => {
  return (
    <aside className="preview-pane" aria-label="Podgląd zaznaczonego wpisu">
      <header className="preview-pane__header">
        <h2>{preview ? KIND_LABELS[preview.kind] : 'Zaznacz wpis'}</h2>
        {onClose ? (
          <button
            type="button"
            className="preview-pane__close"
            aria-label="Zamknij podgląd"
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
            <span>Wczytywanie podglądu…</span>
          </div>
        ) : null}
        {status === 'error' ? (
          <div className="preview-placeholder" role="alert">
            <Eye size={24} aria-hidden="true" />
            <strong>Nie udało się wczytać podglądu</strong>
            <span>Wybierz wpis ponownie lub spróbuj później.</span>
          </div>
        ) : null}
        {status === 'idle' ? (
          <div className="preview-placeholder">
            <Eye size={24} aria-hidden="true" />
            <span>Wybierz wpis z historii.</span>
          </div>
        ) : null}
        {status === 'ready' && preview ? (
          <>
            {preview.kind === 'image' ? (
              <ImagePreview
                thumbnailUrl={thumbnailUrl}
                thumbnailStatus={thumbnailStatus}
              />
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
                <dt>Źródło</dt>
                <dd>{preview.sourceAppName ?? 'Nieznana aplikacja'}</dd>
              </div>
              <div>
                <dt>Rozmiar</dt>
                <dd>{formatByteSize(preview.byteSize)}</dd>
              </div>
            </dl>
          </>
        ) : null}
      </div>
      {actions}
    </aside>
  );
};
