import { useVirtualizer } from '@tanstack/react-virtual';
import { useEffect, useRef } from 'react';

import type { AppEntry } from '../lib/contracts';
import { AppRow } from './AppRow';

interface AppsListProps {
  id?: string;
  apps: AppEntry[];
  selectedKey: string | null;
  onSelect: (path: string) => void;
  onActivate: (path: string) => void;
}

/// The launcher's application list: a virtualized listbox over the filtered
/// catalog, the exact structure of the history list with a different item
/// behind it. The catalog is bounded (two thousand entries at most), but the
/// DOM stays bounded at any size, exactly like the history it sits beside.
export const AppsList = ({
  id = 'apps-results',
  apps,
  selectedKey,
  onSelect,
  onActivate,
}: AppsListProps): React.JSX.Element => {
  const scrollRef = useRef<HTMLDivElement>(null);
  const virtualizer = useVirtualizer({
    count: apps.length,
    getScrollElement: () => scrollRef.current,
    estimateSize: () => 58,
    overscan: 8,
    getItemKey: (index) => apps[index]?.path ?? index,
    initialRect: { width: 800, height: 420 },
  });

  useEffect(() => {
    const selectedIndex = apps.findIndex((app) => app.path === selectedKey);
    if (selectedIndex >= 0 && typeof scrollRef.current?.scrollTo === 'function') {
      virtualizer.scrollToIndex(selectedIndex, { align: 'auto' });
    }
  }, [apps, selectedKey, virtualizer]);

  return (
    <div
      ref={scrollRef}
      id={id}
      role="listbox"
      aria-label="Application results"
      className="history-list apps-list"
    >
      <div
        className="history-list__canvas"
        style={{ height: `${virtualizer.getTotalSize()}px` }}
      >
        {virtualizer.getVirtualItems().map((virtualItem) => {
          const app = apps[virtualItem.index];
          if (!app) return null;

          return (
            <AppRow
              key={app.path}
              app={app}
              index={virtualItem.index}
              selected={app.path === selectedKey}
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
