import { AppWindowMac } from 'lucide-react';
import type { CSSProperties, MouseEventHandler } from 'react';

import { useAppIcon } from '../hooks/useAppIcon';
import { t } from '../i18n';
import type { AppEntry } from '../lib/contracts';
import { useGateway } from '../lib/gateway';

interface AppRowProps {
  app: AppEntry;
  /** Position in the filtered list; the option id is announced from it. */
  index: number;
  selected: boolean;
  style: CSSProperties;
  onSelect: (path: string) => void;
  onActivate: (path: string) => void;
}

/// The icon slot of one application row: the application's own rendered
/// icon when the core has sent it, the placeholder glyph until then.
///
/// A component of its own because the fetch is per-row — the virtualized
/// list mounts only the rows on screen, so only those ask, and the
/// module-wide cache in useAppIcon keeps a remount from asking again.
const AppIconSlot = ({ path }: { path: string }): React.JSX.Element => {
  const gateway = useGateway();
  const { url } = useAppIcon(gateway, path);
  if (url === null) {
    return <AppWindowMac size={17} strokeWidth={1.8} />;
  }
  // Decorative on purpose: the row's accessible name is the application's
  // name, and an icon that repeated it would be read twice.
  return <img className="app-row__icon" src={url} alt="" />;
};

/// One launchable application in the launcher list.
///
/// Deliberately the same row rhythm as a history entry — same height, same
/// icon slot, same metadata line — so the two sit in one palette as
/// siblings rather than as two interfaces stitched together.
export const AppRow = ({
  app,
  index,
  selected,
  style,
  onSelect,
  onActivate,
}: AppRowProps): React.JSX.Element => {
  const handleClick: MouseEventHandler<HTMLDivElement> = () => {
    onSelect(app.path);
  };
  const handleDoubleClick: MouseEventHandler<HTMLDivElement> = () => {
    onActivate(app.path);
  };
  // The folder the bundle sits in, not the whole path: "Utilities" says more
  // than "/System/Applications/Utilities" in the space a metadata line has.
  const container = app.path.split('/').filter(Boolean).at(-2) ?? '';

  return (
    <div
      id={`app-option-${index}`}
      role="option"
      aria-selected={selected}
      aria-label={t('app.row.label', { name: app.name })}
      data-path={app.path}
      className={`history-row app-row${selected ? ' is-selected' : ''}`}
      style={style}
      onClick={handleClick}
      onDoubleClick={handleDoubleClick}
    >
      <span className="history-row__kind" aria-hidden="true">
        <AppIconSlot path={app.path} />
      </span>
      <span className="history-row__content">
        <span className="history-row__preview">{app.name}</span>
        <span className="history-row__metadata">
          <span>{app.bundleId ?? t('app.row.fallback')}</span>
          {container ? <span>{container}</span> : null}
        </span>
      </span>
    </div>
  );
};
