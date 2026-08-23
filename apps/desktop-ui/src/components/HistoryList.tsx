import { useVirtualizer } from '@tanstack/react-virtual';
import { useEffect, useRef } from 'react';

import type { HistoryItem } from '../lib/contracts';
import { HistoryRow } from './HistoryRow';

interface HistoryListProps {
  id?: string;
  items: HistoryItem[];
  selectedId: number | null;
  onSelect: (eventId: number) => void;
  onActivate: (eventId: number) => void;
}

const ROW_HEIGHT = 58;

export const HistoryList = ({
  id = 'history-results',
  items,
  selectedId,
  onSelect,
  onActivate,
}: HistoryListProps): React.JSX.Element => {
  const scrollRef = useRef<HTMLDivElement>(null);
  const virtualizer = useVirtualizer({
    count: items.length,
    getScrollElement: () => scrollRef.current,
    estimateSize: () => ROW_HEIGHT,
    overscan: 8,
    getItemKey: (index) => items[index]?.eventId ?? index,
    initialRect: { width: 800, height: 420 },
  });

  useEffect(() => {
    const selectedIndex = items.findIndex((item) => item.eventId === selectedId);
    if (selectedIndex >= 0 && typeof scrollRef.current?.scrollTo === 'function') {
      virtualizer.scrollToIndex(selectedIndex, { align: 'auto' });
    }
  }, [items, selectedId, virtualizer]);

  return (
    <div
      ref={scrollRef}
      id={id}
      role="listbox"
      aria-label="Wyniki historii schowka"
      className="history-list"
    >
      <div
        className="history-list__canvas"
        style={{ height: `${virtualizer.getTotalSize()}px` }}
      >
        {virtualizer.getVirtualItems().map((virtualItem) => {
          const item = items[virtualItem.index];
          if (!item) return null;

          return (
            <HistoryRow
              key={item.eventId}
              item={item}
              selected={item.eventId === selectedId}
              style={{
                height: `${virtualItem.size}px`,
                transform: `translateY(${virtualItem.start}px)`,
              }}
              onSelect={onSelect}
              onActivate={onActivate}
            />
          );
        })}
      </div>
    </div>
  );
};
