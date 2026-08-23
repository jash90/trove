import { useEffect, useState, type KeyboardEvent } from 'react';

import type { HistoryItem } from '../lib/contracts';

interface UseKeyboardNavigationOptions {
  items: HistoryItem[];
  onActivate: (eventId: number) => void | Promise<void>;
  onEscape: () => void;
}

interface UseKeyboardNavigationResult {
  selectedId: number | null;
  setSelectedId: (eventId: number) => void;
  handleKeyDown: (event: KeyboardEvent<HTMLElement>) => void;
}

export const useKeyboardNavigation = ({
  items,
  onActivate,
  onEscape,
}: UseKeyboardNavigationOptions): UseKeyboardNavigationResult => {
  const [storedSelectedId, setStoredSelectedId] = useState<number | null>(
    items[0]?.eventId ?? null,
  );
  const selectedId = items.some((item) => item.eventId === storedSelectedId)
    ? storedSelectedId
    : (items[0]?.eventId ?? null);

  // Remember only a real selection. Writing the fallback back in would erase
  // what the user picked the moment a query narrows past it, so widening the
  // query again would land on the first row instead of where they were.
  useEffect(() => {
    if (selectedId !== null && storedSelectedId !== selectedId) {
      setStoredSelectedId(selectedId);
    }
  }, [selectedId, storedSelectedId]);

  const handleKeyDown = (event: KeyboardEvent<HTMLElement>): void => {
    const currentIndex = items.findIndex((item) => item.eventId === selectedId);
    let nextIndex: number | null = null;

    switch (event.key) {
      case 'ArrowDown':
        nextIndex = Math.min(Math.max(currentIndex, 0) + 1, items.length - 1);
        break;
      case 'ArrowUp':
        nextIndex = Math.max(currentIndex - 1, 0);
        break;
      case 'Home':
        nextIndex = 0;
        break;
      case 'End':
        nextIndex = items.length - 1;
        break;
      case 'Enter':
        if (selectedId !== null) {
          event.preventDefault();
          void onActivate(selectedId);
        }
        return;
      case 'Escape':
        event.preventDefault();
        onEscape();
        return;
      default:
        return;
    }

    if (nextIndex >= 0 && items[nextIndex]) {
      event.preventDefault();
      setStoredSelectedId(items[nextIndex].eventId);
    }
  };

  return {
    selectedId,
    setSelectedId: setStoredSelectedId,
    handleKeyDown,
  };
};
