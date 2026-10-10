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
import { useT } from '../i18n';
import { fileBasename, formatCapturedAt, kindLabel } from '../lib/format';

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

export const HistoryRow = ({
  item,
  selected,
  style,
  onSelect,
  onActivate,
}: HistoryRowProps): React.JSX.Element => {
  const t = useT();
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
      aria-label={`${kindLabel(item.kind)}: ${displayPreview}${item.occurrenceCount > 1 ? t('history.row.capturedSuffix', { count: item.occurrenceCount }) : ''}`}
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
        <span className="history-row__preview">{displayPreview || t('history.row.noPreview')}</span>
        <span className="history-row__metadata">
          <span>{kindLabel(item.kind)}</span>
          <span>{item.sourceAppName ?? t('history.row.unknownApp')}</span>
        </span>
      </span>
      <span className="history-row__aside">
        {item.pinned ? <Pin size={13} fill="currentColor" aria-label={t('history.row.pinned')} /> : null}
        {item.occurrenceCount > 1 ? (
          <span className="history-row__occurrences" aria-label={t('history.row.captured', { count: item.occurrenceCount })}>
            ×{item.occurrenceCount}
          </span>
        ) : null}
        <time dateTime={new Date(item.capturedAtMs).toISOString()}>
          {formatCapturedAt(item.capturedAtMs)}
        </time>
      </span>
    </div>
  );
};
