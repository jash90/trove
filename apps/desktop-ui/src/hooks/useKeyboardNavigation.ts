import type { KeyboardEvent } from 'react';

import type { HistoryItem } from '../lib/contracts';
import { useListNavigation } from './useListNavigation';

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

/**
 * The history list's keyboard navigation, now a thin wrapper over the shared
 * `useListNavigation`. The public shape is unchanged — numeric event ids in,
 * numeric event ids out — so the palette and every existing test keep
 * working while the launcher list moves by the same rules.
 */
export const useKeyboardNavigation = ({
  items,
  onActivate,
  onEscape,
}: UseKeyboardNavigationOptions): UseKeyboardNavigationResult => {
  const navigation = useListNavigation({
    items,
    keyOf: (item) => String(item.eventId),
    onActivate: (item) => onActivate(item.eventId),
    onEscape,
  });

  return {
    selectedId: navigation.selectedKey === null ? null : Number(navigation.selectedKey),
    setSelectedId: (eventId) => navigation.setSelectedKey(String(eventId)),
    handleKeyDown: navigation.handleKeyDown,
  };
};
