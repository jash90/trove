import { useMemo, useState } from 'react';

import { t } from '../i18n';
import type { CopyMode, HistoryItem } from '../lib/contracts';
import type { ClipboardGateway } from '../lib/gateway';

export type HistoryActionIntent = 'copy' | 'paste' | 'pastePlain';

const copyFeedback = (mode: CopyMode, intent: HistoryActionIntent): string => {
  if (mode === 'pasted') {
    return intent === 'pastePlain'
      ? t('feedback.pastedPlain')
      : t('feedback.pasted');
  }
  if (mode === 'copied') {
    if (intent === 'paste') {
      return t('feedback.copiedFallback');
    }
    if (intent === 'pastePlain') {
      return t('feedback.copiedPlainFallback');
    }
    return t('feedback.copied');
  }
  // The three refusals used to share one sentence, which made the one that has
  // a fix look the same as the two that do not. Only the first is something the
  // user can act on, and it is by far the most common.
  if (mode === 'copied_only_permission_required') {
    return t('feedback.needsPermission');
  }
  if (mode === 'copied_only_target_lost') {
    return t('feedback.targetLost');
  }
  return t('feedback.cannotPaste');
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
  /**
   * The outcome the feedback describes, so the interface can offer the one
   * refusal that has a fix a way to reach it. Null whenever `feedback` is.
   */
  feedbackMode: CopyMode | null;
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
  const [feedbackMode, setFeedbackMode] = useState<CopyMode | null>(null);

  const clearFeedback = (): void => {
    setFeedback(null);
    setFeedbackMode(null);
  };

  /** A message with no paste outcome behind it, so no offer of a fix. */
  const report = (message: string): void => {
    setFeedback(message);
    setFeedbackMode(null);
  };

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
    clearFeedback();
    void gateway
      .copyEvent(eventId, plainText, intent !== 'copy')
      .then((result) => {
        setFeedback(copyFeedback(result.mode, intent));
        setFeedbackMode(result.mode);
      })
      .catch(() => report(t('feedback.copyFailed')));
  };

  const togglePin = (item: HistoryItem): void => {
    if (pinPendingId !== null) return;
    const eventId = item.eventId;
    const previousPinned = item.pinned;
    const nextPinned = !previousPinned;
    clearFeedback();
    setPinPendingId(eventId);
    setPinOverrides((current) => ({ ...current, [eventId]: nextPinned }));
    void gateway
      .setPinned(eventId, nextPinned)
      .catch(() => {
        setPinOverrides((current) => ({ ...current, [eventId]: previousPinned }));
        report(t('feedback.pinFailed'));
      })
      .finally(() => setPinPendingId(null));
  };

  const requestDelete = (eventId: number): void => {
    clearFeedback();
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
        report(t('feedback.deleted'));
      })
      .catch(() => {
        setDeleteTargetId(null);
        report(t('feedback.deleteFailed'));
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
    feedbackMode,
    clearFeedback,
    copy,
    togglePin,
    requestDelete,
    cancelDelete,
    confirmDelete,
  };
};
