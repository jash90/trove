import { useEffect, useState, type KeyboardEvent } from 'react';

interface UseListNavigationOptions<T> {
  items: T[];
  /** The stable identity of one item: an event id as a string, an app path. */
  keyOf: (item: T) => string;
  onActivate: (item: T) => void | Promise<void>;
  onEscape: () => void;
}

interface UseListNavigationResult {
  selectedKey: string | null;
  setSelectedKey: (key: string) => void;
  handleKeyDown: (event: KeyboardEvent<HTMLElement>) => void;
}

/**
 * Keyboard navigation over a list of anything with a stable key: arrows,
 * Home, End, Enter to activate the selection, Escape delegated upward.
 *
 * Extracted from the history list's hook when the launcher needed the same
 * movement over a different item type; the history side wraps it rather than
 * duplicating it, so both lists move the same way because they share one
 * implementation, not because someone kept them in step by hand.
 */
export const useListNavigation = <T,>({
  items,
  keyOf,
  onActivate,
  onEscape,
}: UseListNavigationOptions<T>): UseListNavigationResult => {
  const [storedSelectedKey, setStoredSelectedKey] = useState<string | null>(
    items.length > 0 ? (keyOf(items[0]!) ?? null) : null,
  );
  const selectedKey = items.some((item) => keyOf(item) === storedSelectedKey)
    ? storedSelectedKey
    : (items.length > 0 ? keyOf(items[0]!) : null);

  // Remember only a real selection. Writing the fallback back in would erase
  // what the user picked the moment a query narrows past it, so widening the
  // query again would land on the first row instead of where they were.
  useEffect(() => {
    if (selectedKey !== null && storedSelectedKey !== selectedKey) {
      setStoredSelectedKey(selectedKey);
    }
  }, [selectedKey, storedSelectedKey]);

  const handleKeyDown = (event: KeyboardEvent<HTMLElement>): void => {
    const currentIndex = items.findIndex((item) => keyOf(item) === selectedKey);
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
        if (selectedKey !== null) {
          event.preventDefault();
          const item = items.find((candidate) => keyOf(candidate) === selectedKey);
          if (item !== undefined) void onActivate(item);
        }
        return;
      case 'Escape':
        event.preventDefault();
        onEscape();
        return;
      default:
        return;
    }

    if (nextIndex !== null && nextIndex >= 0 && items[nextIndex]) {
      event.preventDefault();
      setStoredSelectedKey(keyOf(items[nextIndex]));
    }
  };

  return {
    selectedKey,
    setSelectedKey: setStoredSelectedKey,
    handleKeyDown,
  };
};
