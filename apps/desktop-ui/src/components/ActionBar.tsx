import { ClipboardCopy, Pin, PinOff, ShieldCheck, Trash2, Type } from 'lucide-react';
import { useRef, type KeyboardEventHandler } from 'react';
import { createPortal } from 'react-dom';

import { useModalFocus } from '../hooks/useModalFocus';
import type { CopyMode } from '../lib/contracts';

interface ActionBarProps {
  pinned: boolean;
  pinPending: boolean;
  deletePending: boolean;
  feedback: string | null;
  /** The outcome `feedback` describes, when a copy or paste produced it. */
  feedbackMode: CopyMode | null;
  deleteConfirmationOpen: boolean;
  onPaste: () => void;
  onPastePlainText: () => void;
  onTogglePin: () => void;
  onRequestDelete: () => void;
  onCancelDelete: () => void;
  onConfirmDelete: () => void;
  onGrantPastePermission: () => void;
}

export const ActionBar = ({
  pinned,
  pinPending,
  deletePending,
  feedback,
  feedbackMode,
  deleteConfirmationOpen,
  onPaste,
  onPastePlainText,
  onTogglePin,
  onRequestDelete,
  onCancelDelete,
  onConfirmDelete,
  onGrantPastePermission,
}: ActionBarProps): React.JSX.Element => {
  const cancelRef = useRef<HTMLButtonElement>(null);
  const deleteInvokerRef = useRef<HTMLButtonElement>(null);
  // A confirmed delete removes the row the invoker acted on, so the caller
  // decides where focus lands; only cancellation returns it to the button.
  const suppressReturnFocusRef = useRef(false);
  const modalFocus = useModalFocus({
    active: deleteConfirmationOpen,
    initialFocusRef: cancelRef,
    returnFocusRef: deleteInvokerRef,
    suppressReturnFocusRef,
  });

  const handleCancelDelete = (): void => {
    suppressReturnFocusRef.current = false;
    onCancelDelete();
  };

  const handleConfirmDelete = (): void => {
    suppressReturnFocusRef.current = true;
    onConfirmDelete();
  };

  const handleDialogKeyDown: KeyboardEventHandler<HTMLDivElement> = (event) => {
    modalFocus.onKeyDown(event);
    if (event.key === 'Escape') {
      event.preventDefault();
      handleCancelDelete();
    }
  };

  const confirmation = deleteConfirmationOpen ? (
    <div className="confirmation-backdrop">
      <div
        className="confirmation-sheet"
        role="dialog"
        aria-modal="true"
        aria-labelledby="delete-confirmation-title"
        aria-describedby="delete-confirmation-description"
        onKeyDown={handleDialogKeyDown}
      >
        <span className="preview-label">Irreversible action</span>
        <h2 id="delete-confirmation-title">Delete this entry from the history?</h2>
        <p id="delete-confirmation-description">
          This entry cannot be restored.
        </p>
        <div className="confirmation-sheet__actions">
          <button ref={cancelRef} type="button" disabled={deletePending} onClick={handleCancelDelete}>
            Cancel deletion
          </button>
          <button
            type="button"
            className="confirmation-sheet__danger"
            disabled={deletePending}
            onClick={handleConfirmDelete}
          >
            {deletePending ? 'Deleting…' : 'Delete permanently'}
          </button>
        </div>
      </div>
    </div>
  ) : null;

  return (
    <>
      <div className="action-bar" aria-label="Actions for the selected entry">
        <button type="button" aria-label="Paste or copy the entry" onClick={onPaste}>
          <ClipboardCopy size={15} aria-hidden="true" />
          <span>Paste</span>
        </button>
        <button
          type="button"
          aria-label="Copy as plain text"
          onClick={onPastePlainText}
        >
          <Type size={15} aria-hidden="true" />
          <span>Text</span>
        </button>
        <button
          type="button"
          aria-label={pinned ? 'Unpin entry' : 'Pin entry'}
          aria-pressed={pinned}
          disabled={pinPending}
          onClick={onTogglePin}
        >
          {pinned ? <PinOff size={15} aria-hidden="true" /> : <Pin size={15} aria-hidden="true" />}
          <span>{pinned ? 'Unpin' : 'Pin'}</span>
        </button>
        <button
          ref={deleteInvokerRef}
          type="button"
          className="action-bar__delete"
          aria-label="Delete entry"
          onClick={onRequestDelete}
        >
          <Trash2 size={15} aria-hidden="true" />
          <span>Delete</span>
        </button>
      </div>
      {feedback ? (
        <p className="action-feedback" role="status" aria-live="polite">
          {feedback}
          {/* The only refusal the user can do something about, so it is the
              only one that carries a way to do it. */}
          {feedbackMode === 'copied_only_permission_required' ? (
            <button
              type="button"
              className="action-feedback__fix"
              onClick={onGrantPastePermission}
            >
              <ShieldCheck size={14} aria-hidden="true" />
              <span>Open System Settings</span>
            </button>
          ) : null}
        </p>
      ) : null}
      {confirmation && typeof document !== 'undefined'
        ? createPortal(confirmation, document.body)
        : null}
    </>
  );
};
