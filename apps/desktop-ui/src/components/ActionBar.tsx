import { ClipboardCopy, Pin, PinOff, Trash2, Type } from 'lucide-react';
import { useEffect, useRef, type KeyboardEventHandler } from 'react';

interface ActionBarProps {
  pinned: boolean;
  pinPending: boolean;
  deletePending: boolean;
  feedback: string | null;
  deleteConfirmationOpen: boolean;
  onCopy: () => void;
  onCopyPlainText: () => void;
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
  onCopy,
  onCopyPlainText,
  onTogglePin,
  onRequestDelete,
  onCancelDelete,
  onConfirmDelete,
}: ActionBarProps): React.JSX.Element => {
  const cancelRef = useRef<HTMLButtonElement>(null);

  useEffect(() => {
    if (deleteConfirmationOpen) cancelRef.current?.focus();
  }, [deleteConfirmationOpen]);

  const handleDialogKeyDown: KeyboardEventHandler<HTMLDivElement> = (event) => {
    event.stopPropagation();
    if (event.key === 'Escape') {
      event.preventDefault();
      onCancelDelete();
    }
  };

  return (
    <>
      <div className="action-bar" aria-label="Działania dla zaznaczonego wpisu">
        <button type="button" aria-label="Wklej lub skopiuj wpis" onClick={onCopy}>
          <ClipboardCopy size={15} aria-hidden="true" />
          <span>Wklej</span>
        </button>
        <button
          type="button"
          aria-label="Kopiuj jako zwykły tekst"
          onClick={onCopyPlainText}
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
      {deleteConfirmationOpen ? (
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
              <button ref={cancelRef} type="button" disabled={deletePending} onClick={onCancelDelete}>
                Anuluj usuwanie
              </button>
              <button
                type="button"
                className="confirmation-sheet__danger"
                disabled={deletePending}
                onClick={onConfirmDelete}
              >
                {deletePending ? 'Usuwanie…' : 'Usuń wpis bezpowrotnie'}
              </button>
            </div>
          </div>
        </div>
      ) : null}
    </>
  );
};
