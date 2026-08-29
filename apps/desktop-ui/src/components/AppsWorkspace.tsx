import type { AppEntry } from '../lib/contracts';
import { AppsList } from './AppsList';
import { EmptyState } from './EmptyState';

interface AppsWorkspaceProps {
  status: 'loading' | 'ready' | 'error';
  apps: AppEntry[];
  selectedKey: string | null;
  /** Set when a launch was refused; cleared by the next attempt or selection. */
  launchError: string | null;
  onSelect: (path: string) => void;
  onActivate: (path: string) => void;
}

/// The launcher mode's middle: one column, no preview pane, no action bar.
///
/// An application has no payload to preview and nothing to pin or delete, so
/// the space those occupy in the history mode is simply the list here — the
/// row rhythm carries the visual continuity instead of a second column.
///
/// The list is also what stays on screen while the catalog reloads: swapping
/// it for a loading state on every mode entry would both flash and leave
/// Enter steering at a list nobody can see. The placeholder states appear
/// only when there is nothing to show instead — the same deal the history
/// list gives its `refreshing` flag.
export const AppsWorkspace = ({
  status,
  apps,
  selectedKey,
  launchError,
  onSelect,
  onActivate,
}: AppsWorkspaceProps): React.JSX.Element => (
  <div className="palette-content palette-content--apps">
    {launchError ? (
      <p className="apps-launch-error" role="alert">
        {launchError}
      </p>
    ) : null}
    <div className="history-column">
      <div className="history-panel">
        {apps.length === 0 && status === 'loading' ? (
          <EmptyState kind="loading" subject="applications" />
        ) : null}
        {apps.length === 0 && status === 'error' ? (
          <EmptyState kind="error" subject="applications" />
        ) : null}
        {status === 'ready' && apps.length === 0 ? (
          <EmptyState kind="empty" subject="applications" />
        ) : null}
        {apps.length > 0 ? (
          <AppsList
            apps={apps}
            selectedKey={selectedKey}
            onSelect={onSelect}
            onActivate={onActivate}
          />
        ) : null}
      </div>
    </div>
  </div>
);
