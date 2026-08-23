import { useMemo, useState } from 'react';

import type { CopyMode, HistoryItem } from '../lib/contracts';
import type { ClipboardGateway } from '../lib/gateway';

export type HistoryActionIntent = 'copy' | 'paste' | 'pastePlain';

const copyFeedback = (mode: CopyMode, intent: HistoryActionIntent): string => {
  if (mode === 'pasted') {
    return intent === 'pastePlain'
      ? 'Pasted as plain text.'
      : 'Pasted the selected entry.';
  }
  if (mode === 'copied') {
    if (intent === 'paste') {
      return 'Copied as a fallback — automatic pasting is unavailable.';
    }
    if (intent === 'pastePlain') {
      return 'Copied as plain text — automatic pasting is unavailable.';
    }
    return 'Copied to the clipboard.';
  }
  return 'Copied. Automatic pasting is not available.';
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
      .catch(() => setFeedback('The entry could not be copied.'));
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
        setFeedback('The pin could not be changed.');
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
        setFeedback('Entry deleted from the history.');
      })
      .catch(() => {
        setDeleteTargetId(null);
        setFeedback('The entry could not be deleted.');
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
