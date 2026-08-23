import { useMemo, useState } from 'react';

import type { CopyMode, HistoryItem } from '../lib/contracts';
import type { ClipboardGateway } from '../lib/gateway';

export type HistoryActionIntent = 'copy' | 'paste' | 'pastePlain';

const copyFeedback = (mode: CopyMode, intent: HistoryActionIntent): string => {
  if (mode === 'pasted') {
    return intent === 'pastePlain'
      ? 'Wklejono jako zwykły tekst.'
      : 'Wklejono zaznaczony wpis.';
  }
  if (mode === 'copied') {
    if (intent === 'paste') {
      return 'Skopiowano jako fallback — automatyczne wklejenie jest niedostępne.';
    }
    if (intent === 'pastePlain') {
      return 'Skopiowano jako zwykły tekst — automatyczne wklejenie jest niedostępne.';
    }
    return 'Skopiowano do schowka.';
  }
  return 'Skopiowano. Automatyczne wklejenie nie jest dostępne.';
};

interface UseHistoryActionsOptions {
  gateway: ClipboardGateway;
  items: HistoryItem[];
  onFocusSearch: () => void;
  onOpenPreview: () => void;
}

interface UseHistoryActionsResult {
  visibleItems: HistoryItem[];
  pinPendingId: number | null;
  deleteTargetId: number | null;
  deletePending: boolean;
  feedback: string | null;
  clearFeedback: () => void;
  copy: (eventId: number, intent: HistoryActionIntent) => void;
  togglePin: (item: HistoryItem) => void;
  requestDelete: (eventId: number) => void;
  cancelDelete: () => void;
  confirmDelete: () => void;
}

export const useHistoryActions = ({
  gateway,
  items,
  onFocusSearch,
  onOpenPreview,
}: UseHistoryActionsOptions): UseHistoryActionsResult => {
  const [pinOverrides, setPinOverrides] = useState<Record<number, boolean>>({});
  const [deletedIds, setDeletedIds] = useState<ReadonlySet<number>>(() => new Set());
  const [pinPendingId, setPinPendingId] = useState<number | null>(null);
  const [deleteTargetId, setDeleteTargetId] = useState<number | null>(null);
  const [deletePending, setDeletePending] = useState(false);
  const [feedback, setFeedback] = useState<string | null>(null);

  const visibleItems = useMemo(
    () =>
      items
        .filter((item) => !deletedIds.has(item.eventId))
        .map((item): HistoryItem => ({
          ...item,
          pinned: pinOverrides[item.eventId] ?? item.pinned,
        })),
    [deletedIds, items, pinOverrides],
  );

  const copy = (eventId: number, intent: HistoryActionIntent): void => {
    const plainText = intent === 'pastePlain';
    setFeedback(null);
    void gateway
      .copyEvent(eventId, plainText, intent !== 'copy')
      .then((result) => setFeedback(copyFeedback(result.mode, intent)))
      .catch(() => setFeedback('Nie udało się skopiować wpisu.'));
  };

  const togglePin = (item: HistoryItem): void => {
    if (pinPendingId !== null) return;
    const eventId = item.eventId;
    const previousPinned = item.pinned;
    const nextPinned = !previousPinned;
    setFeedback(null);
    setPinPendingId(eventId);
    setPinOverrides((current) => ({ ...current, [eventId]: nextPinned }));
    void gateway
      .setPinned(eventId, nextPinned)
      .catch(() => {
        setPinOverrides((current) => ({ ...current, [eventId]: previousPinned }));
        setFeedback('Nie udało się zmienić przypięcia.');
      })
      .finally(() => setPinPendingId(null));
  };

  const requestDelete = (eventId: number): void => {
    setFeedback(null);
    onOpenPreview();
    setDeleteTargetId(eventId);
  };

  const cancelDelete = (): void => {
    if (deletePending) return;
    setDeleteTargetId(null);
    onFocusSearch();
  };

  const confirmDelete = (): void => {
    if (deleteTargetId === null || deletePending) return;
    const eventId = deleteTargetId;
    setDeletePending(true);
    void gateway
      .deleteEvent(eventId)
      .then(() => {
        setDeletedIds((current) => new Set([...current, eventId]));
        setDeleteTargetId(null);
        setFeedback('Usunięto wpis z historii.');
      })
      .catch(() => {
        setDeleteTargetId(null);
        setFeedback('Nie udało się usunąć wpisu.');
      })
      .finally(() => {
        setDeletePending(false);
        onFocusSearch();
      });
  };

  return {
    visibleItems,
    pinPendingId,
    deleteTargetId,
    deletePending,
    feedback,
    clearFeedback: () => setFeedback(null),
    copy,
    togglePin,
    requestDelete,
    cancelDelete,
    confirmDelete,
  };
};
