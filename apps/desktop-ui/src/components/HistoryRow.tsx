import {
  Braces,
  File,
  FileCode2,
  FileText,
  Image,
  Link2,
  Palette,
  Pin,
  Type,
  type LucideIcon,
} from 'lucide-react';
import type { CSSProperties, MouseEventHandler } from 'react';

import type { ContentKind, HistoryItem } from '../lib/contracts';
import { fileBasename } from '../lib/format';

interface HistoryRowProps {
  item: HistoryItem;
  selected: boolean;
  style: CSSProperties;
  onSelect: (eventId: number) => void;
  onActivate: (eventId: number) => void;
}

const KIND_ICONS: Record<ContentKind, LucideIcon> = {
  text: Type,
  link: Link2,
  image: Image,
  file: File,
  color: Palette,
  code: Braces,
  html: FileCode2,
};

const KIND_LABELS: Record<ContentKind, string> = {
  text: 'Text',
  link: 'Link',
  image: 'Image',
  file: 'File',
  color: 'Colour',
  code: 'Code',
  html: 'HTML',
};

const CAPTURED_AT_FORMATTER = new Intl.DateTimeFormat('en-GB', {
  day: '2-digit',
  month: 'short',
  hour: '2-digit',
  minute: '2-digit',
});

const formatCapturedAt = (capturedAtMs: number): string =>
  CAPTURED_AT_FORMATTER.format(capturedAtMs);

export const HistoryRow = ({
  item,
  selected,
  style,
  onSelect,
  onActivate,
}: HistoryRowProps): React.JSX.Element => {
  const KindIcon = KIND_ICONS[item.kind] ?? FileText;
  const displayPreview = item.kind === 'file' ? fileBasename(item.preview) : item.preview;
  const handleClick: MouseEventHandler<HTMLDivElement> = () => {
    onSelect(item.eventId);
  };
  const handleDoubleClick: MouseEventHandler<HTMLDivElement> = () => {
    onActivate(item.eventId);
  };

  return (
    <div
      id={`history-option-${item.eventId}`}
      role="option"
      aria-selected={selected}
      aria-label={`${KIND_LABELS[item.kind]}: ${displayPreview}`}
      data-event-id={item.eventId}
      className={`history-row${selected ? ' is-selected' : ''}${item.pinned ? ' is-pinned' : ''}`}
      style={style}
      onClick={handleClick}
      onDoubleClick={handleDoubleClick}
    >
      <span className="history-row__kind" aria-hidden="true">
        <KindIcon size={17} strokeWidth={1.8} />
      </span>
      <span className="history-row__content">
        <span className="history-row__preview">{displayPreview || 'Entry with no preview'}</span>
        <span className="history-row__metadata">
          <span>{KIND_LABELS[item.kind]}</span>
          <span>{item.sourceAppName ?? 'Unknown application'}</span>
        </span>
      </span>
      <span className="history-row__aside">
        {item.pinned ? <Pin size={13} fill="currentColor" aria-label="Pinned" /> : null}
        <time dateTime={new Date(item.capturedAtMs).toISOString()}>
          {formatCapturedAt(item.capturedAtMs)}
        </time>
      </span>
    </div>
  );
};
