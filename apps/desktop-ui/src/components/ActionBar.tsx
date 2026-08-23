import { ClipboardCopy, Pin, PinOff, Trash2, Type } from 'lucide-react';
import { useRef, type KeyboardEventHandler } from 'react';
import { createPortal } from 'react-dom';

import { useModalFocus } from '../hooks/useModalFocus';

interface ActionBarProps {
  pinned: boolean;
  pinPending: boolean;
  deletePending: boolean;
  feedback: string | null;
  deleteConfirmationOpen: boolean;
  onPaste: () => void;
  onPastePlainText: () => void;
  onTogglePin: () => void;
  onRequestDelete: () => void;
  onCancelDelete: () => void;
  onConfirmDelete: () => void;
}

export const ActionBar = ({
  pinned,
  pinPending,
  deletePending,
  feedback,
  deleteConfirmationOpen,
  onPaste,
  onPastePlainText,
  onTogglePin,
  onRequestDelete,
  onCancelDelete,
  onConfirmDelete,
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
        <span className="preview-label">Nieodwracalne działanie</span>
        <h2 id="delete-confirmation-title">Usunąć wpis z historii?</h2>
        <p id="delete-confirmation-description">
          Tego wpisu nie będzie można przywrócić.
        </p>
        <div className="confirmation-sheet__actions">
          <button ref={cancelRef} type="button" disabled={deletePending} onClick={handleCancelDelete}>
            Anuluj usuwanie
          </button>
          <button
            type="button"
            className="confirmation-sheet__danger"
            disabled={deletePending}
            onClick={handleConfirmDelete}
          >
            {deletePending ? 'Usuwanie…' : 'Usuń wpis bezpowrotnie'}
          </button>
        </div>
      </div>
    </div>
  ) : null;

  return (
    <>
      <div className="action-bar" aria-label="Działania dla zaznaczonego wpisu">
        <button type="button" aria-label="Wklej lub skopiuj wpis" onClick={onPaste}>
          <ClipboardCopy size={15} aria-hidden="true" />
          <span>Wklej</span>
        </button>
        <button
          type="button"
          aria-label="Kopiuj jako zwykły tekst"
          onClick={onPastePlainText}
        >
          <Type size={15} aria-hidden="true" />
          <span>Tekst</span>
        </button>
        <button
          type="button"
          aria-label={pinned ? 'Odepnij wpis' : 'Przypnij wpis'}
          aria-pressed={pinned}
          disabled={pinPending}
          onClick={onTogglePin}
        >
          {pinned ? <PinOff size={15} aria-hidden="true" /> : <Pin size={15} aria-hidden="true" />}
          <span>{pinned ? 'Odepnij' : 'Przypnij'}</span>
        </button>
        <button
          ref={deleteInvokerRef}
          type="button"
          className="action-bar__delete"
          aria-label="Usuń wpis"
          onClick={onRequestDelete}
        >
          <Trash2 size={15} aria-hidden="true" />
          <span>Usuń</span>
        </button>
      </div>
      {feedback ? (
        <p className="action-feedback" role="status" aria-live="polite">
          {feedback}
        </p>
      ) : null}
      {confirmation && typeof document !== 'undefined'
        ? createPortal(confirmation, document.body)
        : null}
    </>
  );
};
