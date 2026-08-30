import { useVirtualizer } from '@tanstack/react-virtual';
import { useEffect, useRef } from 'react';

import { keyOfItem, type PaletteItem } from '../lib/paletteItems';
import { AppRow } from './AppRow';
import { VaultRow } from './VaultRow';
import { HistoryRow } from './HistoryRow';

interface PaletteListProps {
  items: PaletteItem[];
  /** The shared keyboard selection, whatever kind of row it sits on. */
  selectedKey: string | null;
  onSelect: (entry: PaletteItem) => void;
  onActivate: (entry: PaletteItem) => void;
}

const ROW_HEIGHT = 58;

/// The palette's one list: applications and clipboard history as a single
/// virtualized sequence, in the order `buildPaletteItems` decided. One
/// scroll container, one keyboard selection, one scrollbar — the row kinds
/// differ only in what they render and what activating them does.
export const PaletteList = ({
  items,
  selectedKey,
  onSelect,
  onActivate,
}: PaletteListProps): React.JSX.Element => {
  const scrollRef = useRef<HTMLDivElement>(null);
  const virtualizer = useVirtualizer({
    count: items.length,
    getScrollElement: () => scrollRef.current,
    estimateSize: () => ROW_HEIGHT,
    overscan: 8,
    getItemKey: (index) => {
      const entry = items[index];
      return entry === undefined ? index : keyOfItem(entry);
    },
    initialRect: { width: 800, height: 420 },
  });

  useEffect(() => {
    const selectedIndex = items.findIndex((entry) => keyOfItem(entry) === selectedKey);
    if (selectedIndex >= 0 && typeof scrollRef.current?.scrollTo === 'function') {
      virtualizer.scrollToIndex(selectedIndex, { align: 'auto' });
    }
  }, [items, selectedKey, virtualizer]);

  return (
    <div
      ref={scrollRef}
      id="history-results"
      role="listbox"
      aria-label="Applications, secrets and history results"
      className="history-list"
    >
      <div
        className="history-list__canvas"
        style={{ height: `${virtualizer.getTotalSize()}px` }}
      >
        {virtualizer.getVirtualItems().map((virtualItem) => {
          const entry = items[virtualItem.index];
          if (!entry) return null;

          const style = {
            height: `${virtualItem.size}px`,
            transform: `translateY(${virtualItem.start}px)`,
          };
          if (entry.kind === 'app') {
            return (
              <AppRow
                key={entry.app.path}
                app={entry.app}
                index={virtualItem.index}
                selected={keyOfItem(entry) === selectedKey}
                style={style}
                onSelect={() => onSelect(entry)}
                onActivate={() => onActivate(entry)}
              />
            );
          }
          if (entry.kind === 'vault') {
            return (
              <VaultRow
                key={`v${entry.secret.slug}`}
                secret={entry.secret}
                index={virtualItem.index}
                selected={keyOfItem(entry) === selectedKey}
                style={style}
                onSelect={() => onSelect(entry)}
                onActivate={() => onActivate(entry)}
              />
            );
          }
          return (
            <HistoryRow
              key={`h${entry.item.eventId}`}
              item={entry.item}
              selected={keyOfItem(entry) === selectedKey}
              style={style}
              onSelect={() => onSelect(entry)}
              onActivate={() => onActivate(entry)}
            />
          );
        })}
      </div>
    </div>
  );
};
