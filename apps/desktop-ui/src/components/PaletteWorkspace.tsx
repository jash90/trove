import { PanelRightOpen } from 'lucide-react';

import type { HistoryItem, LinkPreview as LinkPreviewContract, Preview } from '../lib/contracts';
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
  linkPreview: LinkPreviewContract | null;
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
  linkPreview,
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
      {/* Rendered whatever the list holds. Appearing and disappearing with the
          results moved the list's top edge on every keystroke at narrow
          widths, which is its own kind of jumping. */}
      <button
        type="button"
        className="preview-toggle"
        aria-label="Pokaż podgląd zaznaczonego wpisu"
        disabled={items.length === 0}
        onClick={onOpenPreview}
      >
        <PanelRightOpen size={15} aria-hidden="true" />
        Podgląd
      </button>
      <div className="history-panel">
        {/* `loading` now means there is nothing to show yet, so this replaces
            the list once, on first open — never again mid-typing. */}
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
        linkPreview={linkPreview}
        onClose={onClosePreview}
        onRevealSource={onRevealSource}
        actions={actions}
      />
    </div>
  </div>
);
