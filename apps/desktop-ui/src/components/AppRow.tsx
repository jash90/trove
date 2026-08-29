import { AppWindowMac } from 'lucide-react';
import type { CSSProperties, MouseEventHandler } from 'react';

import type { AppEntry } from '../lib/contracts';

interface AppRowProps {
  app: AppEntry;
  /** Position in the filtered list; the option id is announced from it. */
  index: number;
  selected: boolean;
  style: CSSProperties;
  onSelect: (path: string) => void;
  onActivate: (path: string) => void;
}

/// One launchable application in the launcher list.
///
/// Deliberately the same row rhythm as a history entry — same height, same
/// kind icon, same metadata line — so switching modes with Tab moves nothing
/// under the eye; only the content changes. The glyph is a placeholder, not
/// the application's own icon: rendering `.icns` files is a follow-up.
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
      aria-label={`Application: ${app.name}`}
      data-path={app.path}
      className={`history-row app-row${selected ? ' is-selected' : ''}`}
      style={style}
      onClick={handleClick}
      onDoubleClick={handleDoubleClick}
    >
      <span className="history-row__kind" aria-hidden="true">
        <AppWindowMac size={17} strokeWidth={1.8} />
      </span>
      <span className="history-row__content">
        <span className="history-row__preview">{app.name}</span>
        <span className="history-row__metadata">
          <span>{app.bundleId ?? 'Application'}</span>
          {container ? <span>{container}</span> : null}
        </span>
      </span>
    </div>
  );
};
