import { PanelRightOpen } from 'lucide-react';

import type { HistoryItem, LinkPreview as LinkPreviewContract, Preview } from '../lib/contracts';
import type { PaletteItem } from '../lib/paletteItems';
import type { PreviewStatus } from './PreviewPane';
import type { ThumbnailStatus } from './ImagePreview';
import { EmptyState } from './EmptyState';
import { PaletteList } from './PaletteList';
import { PreviewPane } from './PreviewPane';

interface PaletteWorkspaceProps {
  appsStatus: 'loading' | 'ready' | 'error';
  status: 'loading' | 'ready' | 'error';
  /** The single result list: applications and history, already ordered. */
  items: PaletteItem[];
  /** The shared keyboard selection, whatever kind of row it sits on. */
  selectedKey: string | null;
  /** Set when a launch was refused; cleared by the next attempt or selection. */
  launchError: string | null;
  preview: Preview | null;
  previewStatus: PreviewStatus;
  thumbnailUrl: string | null;
  thumbnailStatus: ThumbnailStatus;
  linkPreview: LinkPreviewContract | null;
  selectedItem: HistoryItem | null;
  mobilePreviewOpen: boolean;
  actions: React.ReactNode;
  onSelect: (entry: PaletteItem) => void;
  onActivate: (entry: PaletteItem) => void;
  onOpenPreview: () => void;
  onClosePreview: () => void;
  onRevealSource: () => void;
}

/// The palette's middle: one list of applications and clipboard history,
/// the preview column beside it.
///
/// One field drives the list and one keyboard selection moves through it.
/// Rows already on screen win over state changes — swapping the list for a
/// loading state would both flash and leave Enter steering at rows nobody
/// can see — so the states speak only when there is nothing to show.
export const PaletteWorkspace = ({
  appsStatus,
  status,
  items,
  selectedKey,
  launchError,
  preview,
  previewStatus,
  thumbnailUrl,
  thumbnailStatus,
  linkPreview,
  selectedItem,
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
        aria-label="Show the selected entry preview"
        disabled={items.length === 0}
        onClick={onOpenPreview}
      >
        <PanelRightOpen size={15} aria-hidden="true" />
        Preview
      </button>
      {launchError ? (
        <p className="apps-launch-error" role="alert">
          {launchError}
        </p>
      ) : null}
      <div className="history-panel">
        {/* The history announces; the applications are visible — the states
            below speak only when neither side put a row on screen. */}
        {status === 'error' && appsStatus !== 'ready' ? <EmptyState kind="error" /> : null}
        {status === 'error' && appsStatus === 'ready' && items.length === 0 ? (
          <EmptyState kind="error" />
        ) : null}
        {status === 'loading' && items.length === 0 ? <EmptyState kind="loading" /> : null}
        {status === 'ready' && appsStatus === 'ready' && items.length === 0 ? (
          <EmptyState kind="empty" />
        ) : null}
        {items.length > 0 ? (
          <PaletteList
            items={items}
            selectedKey={selectedKey}
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
        selectedItem={selectedItem}
        onClose={onClosePreview}
        onRevealSource={onRevealSource}
        actions={actions}
      />
    </div>
  </div>
);
