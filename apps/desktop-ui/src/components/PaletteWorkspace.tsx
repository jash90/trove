import { PanelRightOpen } from 'lucide-react';

import type { AppEntry, HistoryItem, LinkPreview as LinkPreviewContract, Preview } from '../lib/contracts';
import type { PreviewStatus } from './PreviewPane';
import type { ThumbnailStatus } from './ImagePreview';
import { AppsList } from './AppsList';
import { EmptyState } from './EmptyState';
import { HistoryList } from './HistoryList';
import { PreviewPane } from './PreviewPane';

interface PaletteWorkspaceProps {
  appsStatus: 'loading' | 'ready' | 'error';
  apps: AppEntry[];
  /** Which application row the shared keyboard selection sits on, if any. */
  selectedAppPath: string | null;
  /** Set when a launch was refused; cleared by the next attempt or selection. */
  launchError: string | null;
  status: 'loading' | 'ready' | 'error';
  items: HistoryItem[];
  /** Which history row the shared keyboard selection sits on, if any. */
  selectedId: number | null;
  preview: Preview | null;
  previewStatus: PreviewStatus;
  thumbnailUrl: string | null;
  thumbnailStatus: ThumbnailStatus;
  linkPreview: LinkPreviewContract | null;
  mobilePreviewOpen: boolean;
  actions: React.ReactNode;
  onSelectApp: (path: string) => void;
  onActivateApp: (path: string) => void;
  onSelect: (eventId: number) => void;
  onActivate: (eventId: number) => void;
  onOpenPreview: () => void;
  onClosePreview: () => void;
  onRevealSource: () => void;
}

/// The palette's middle: applications above, history underneath, the
/// preview column beside both.
///
/// One field drives the two lists and one keyboard selection moves through
/// them as a single sequence — applications first, because a launcher is
/// what the palette becomes the moment it opens. The applications section
/// keeps its own bounded height with its own scroll, so a large catalog
/// cannot push the history out of sight; the history takes the rest.
export const PaletteWorkspace = ({
  appsStatus,
  apps,
  selectedAppPath,
  launchError,
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
  onSelectApp,
  onActivateApp,
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
      <section className="palette-section palette-section--apps" aria-label="Aplikacje">
        <h2 className="palette-section__label">Aplikacje</h2>
        <div className="palette-section__panel">
          {/* Quiet on purpose: the history announces, the applications are
              visible — two live regions at once is noise, not information. */}
          {appsStatus === 'loading' && apps.length === 0 ? (
            <EmptyState kind="loading" subject="applications" quiet />
          ) : null}
          {appsStatus === 'error' && apps.length === 0 ? (
            <EmptyState kind="error" subject="applications" quiet />
          ) : null}
          {appsStatus === 'ready' && apps.length === 0 ? (
            <EmptyState kind="empty" subject="applications" quiet />
          ) : null}
          {/* The list is also what stays on screen while the catalog
              reloads: swapping it for a loading state would both flash and
              leave Enter steering at rows nobody can see. */}
          {apps.length > 0 ? (
            <AppsList
              apps={apps}
              selectedKey={selectedAppPath}
              onSelect={onSelectApp}
              onActivate={onActivateApp}
            />
          ) : null}
        </div>
      </section>
      <section className="palette-section palette-section--history" aria-label="Historia">
        <h2 className="palette-section__label">Historia</h2>
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
      </section>
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
