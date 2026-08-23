import { PanelRightOpen } from 'lucide-react';

import type { HistoryItem, Preview } from '../lib/contracts';
import type { PreviewStatus } from './PreviewPane';
import type { ThumbnailStatus } from './ImagePreview';
import { EmptyState } from './EmptyState';
import { HistoryList } from './HistoryList';
import { PreviewPane } from './PreviewPane';

interface PaletteWorkspaceProps {
  status: 'loading' | 'ready' | 'error';
  items: HistoryItem[];
  selectedId: number | null;
  preview: Preview | null;
  previewStatus: PreviewStatus;
  thumbnailUrl: string | null;
  thumbnailStatus: ThumbnailStatus;
  mobilePreviewOpen: boolean;
  actions: React.ReactNode;
  onSelect: (eventId: number) => void;
  onActivate: (eventId: number) => void;
  onOpenPreview: () => void;
  onClosePreview: () => void;
  onRevealSource: () => void;
}

export const PaletteWorkspace = ({
  status,
  items,
  selectedId,
  preview,
  previewStatus,
  thumbnailUrl,
  thumbnailStatus,
  mobilePreviewOpen,
  actions,
  onSelect,
  onActivate,
  onOpenPreview,
  onClosePreview,
  onRevealSource,
}: PaletteWorkspaceProps): React.JSX.Element => (
  <div className="palette-content">
    <div className="history-column">
      {items.length > 0 ? (
        <button
          type="button"
          className="preview-toggle"
          aria-label="Pokaż podgląd zaznaczonego wpisu"
          onClick={onOpenPreview}
        >
          <PanelRightOpen size={15} aria-hidden="true" />
          Podgląd
        </button>
      ) : null}
      <div className="history-panel">
        {status === 'loading' ? <EmptyState kind="loading" /> : null}
        {status === 'error' ? <EmptyState kind="error" /> : null}
        {status === 'ready' && items.length === 0 ? <EmptyState kind="empty" /> : null}
        {status === 'ready' && items.length > 0 ? (
          <HistoryList
            items={items}
            selectedId={selectedId}
            onSelect={onSelect}
            onActivate={onActivate}
          />
        ) : null}
      </div>
    </div>
    <div className={`preview-column${mobilePreviewOpen ? ' is-mobile-open' : ''}`}>
      <PreviewPane
        preview={preview}
        status={previewStatus}
        thumbnailUrl={thumbnailUrl}
        thumbnailStatus={thumbnailStatus}
        onClose={onClosePreview}
        onRevealSource={onRevealSource}
        actions={actions}
      />
    </div>
  </div>
);
