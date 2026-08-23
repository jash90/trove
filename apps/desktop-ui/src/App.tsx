import {
  useEffect,
  useRef,
  useState,
  type KeyboardEvent as ReactKeyboardEvent,
  type KeyboardEventHandler,
} from 'react';

import { ActionBar } from './components/ActionBar';
import { ImportWizard } from './components/ImportWizard';
import { PaletteHeader } from './components/PaletteHeader';
import { PaletteWorkspace } from './components/PaletteWorkspace';
import { useHistoryActions } from './hooks/useHistoryActions';
import { useHistorySearch } from './hooks/useHistorySearch';
import { useKeyboardNavigation } from './hooks/useKeyboardNavigation';
import { useSelectedPreview } from './hooks/useSelectedPreview';
import { useLinkPreview } from './hooks/useLinkPreview';
import { useThumbnail } from './hooks/useThumbnail';
import { HISTORY_PAGE_SIZE } from './lib/contracts';
import {
  GatewayProvider,
  useGateway,
  type ClipboardGateway,
} from './lib/gateway';

interface AppProps {
  gateway?: ClipboardGateway;
}

/** Keys that belong to whatever text field has focus, never to the palette. */
const TEXT_EDITING_KEYS = new Set(['Backspace', 'Delete']);

const shortcutIsBlocked = (
  event: ReactKeyboardEvent<HTMLElement>,
  dialogOpen: boolean,
): boolean => {
  if (dialogOpen) return true;
  const target = event.target;
  if (!(target instanceof HTMLElement)) return false;
  if (target.closest('[role="dialog"], [data-palette-shortcuts="disabled"]')) return true;
  const ownsTextInput =
    target instanceof HTMLInputElement ||
    target instanceof HTMLTextAreaElement ||
    target instanceof HTMLSelectElement ||
    target.isContentEditable;
  if (!ownsTextInput) return false;
  // The query field keeps arrow, Enter and Escape navigation so the user can
  // drive the list without leaving it — but it never surrenders the keys that
  // edit its own text. Backspace there means "erase a character", and letting
  // it reach the palette proposes deleting a history entry instead.
  return target.id !== 'history-search' || TEXT_EDITING_KEYS.has(event.key);
};

const ClipboardPalette = (): React.JSX.Element => {
  const gateway = useGateway();
  const { query, setQuery, status, refreshing, items } = useHistorySearch(gateway);
  const searchInputRef = useRef<HTMLInputElement>(null);
  const [mobilePreviewOpen, setMobilePreviewOpen] = useState(false);
  const [workspace, setWorkspace] = useState<'none' | 'import'>('none');
  const focusSearch = (): void => searchInputRef.current?.focus();
  useEffect(() => {
    searchInputRef.current?.focus();
  }, []);
  const actions = useHistoryActions({
    gateway,
    items,
    onFocusSearch: focusSearch,
    onOpenPreview: () => setMobilePreviewOpen(true),
  });
  const handleActivate = (eventId: number): void => actions.copy(eventId, 'paste');
  const navigation = useKeyboardNavigation({
    items: actions.visibleItems,
    onActivate: handleActivate,
    onEscape: () => setQuery(''),
  });
  const selectedItem =
    actions.visibleItems.find((item) => item.eventId === navigation.selectedId) ?? null;
  const preview = useSelectedPreview(gateway, navigation.selectedId);
  const thumbnail = useThumbnail(
    gateway,
    navigation.selectedId,
    // Not gated on `hasThumbnail`: that flag is false exactly while no
    // thumbnail exists, which is when one needs rendering. The command
    // answers cheaply when there is no image to render from.
    selectedItem?.kind === 'image',
  );
  const link = useLinkPreview(gateway, navigation.selectedId, selectedItem?.kind === 'link');

  // Settings are their own window; the palette only asks for it.
  const openSettings = (): void => void gateway.openSettingsWindow().catch(() => undefined);

  const modalOpen = actions.deleteTargetId !== null || workspace !== 'none';

  // Only one workspace at a time. Focus returns to the search field rather
  // than to whatever opened the dialog: the palette has one place a keyboard
  // user works from, and a shortcut has no button to go back to.
  const closeWorkspace = (): void => {
    setWorkspace('none');
    queueMicrotask(focusSearch);
  };

  const handleSelect = (eventId: number): void => {
    navigation.setSelectedId(eventId);
    actions.clearFeedback();
    focusSearch();
  };
  const handleQueryChange = (nextQuery: string): void => {
    setQuery(nextQuery);
    focusSearch();
  };
  const handleSearchKeyDown: KeyboardEventHandler<HTMLInputElement> = (event) => {
    if (actions.deleteTargetId === null) navigation.handleKeyDown(event);
  };
  const handleCopy = (): void => {
    if (navigation.selectedId !== null) actions.copy(navigation.selectedId, 'copy');
  };
  const handlePaste = (): void => {
    if (navigation.selectedId !== null) actions.copy(navigation.selectedId, 'paste');
  };
  const handlePastePlainText = (): void => {
    if (navigation.selectedId !== null) actions.copy(navigation.selectedId, 'pastePlain');
  };
  const handleTogglePin = (): void => {
    if (selectedItem) actions.togglePin(selectedItem);
  };
  const handleRequestDelete = (): void => {
    if (navigation.selectedId !== null) actions.requestDelete(navigation.selectedId);
  };
  const handlePaletteKeyDown: KeyboardEventHandler<HTMLElement> = (event) => {
    if (shortcutIsBlocked(event, modalOpen)) return;
    const key = event.key.toLocaleLowerCase('en-US');
    const primaryModifier = event.metaKey || event.ctrlKey;
    // These two open a dialog rather than act on a row, so they work with an
    // empty history — which is exactly when someone reaches for the importer.
    if (primaryModifier && !event.shiftKey && key === 'i') {
      event.preventDefault();
      setWorkspace('import');
      return;
    }
    if (primaryModifier && !event.shiftKey && key === ',') {
      event.preventDefault();
      openSettings();
      return;
    }
    if (navigation.selectedId === null) return;
    if (primaryModifier && !event.shiftKey && key === 'c') {
      event.preventDefault();
      handleCopy();
    } else if (primaryModifier && event.shiftKey && key === 'v') {
      event.preventDefault();
      handlePastePlainText();
    } else if (primaryModifier && !event.shiftKey && key === 'p') {
      event.preventDefault();
      handleTogglePin();
    } else if (
      !primaryModifier &&
      !event.altKey &&
      (event.key === 'Backspace' || event.key === 'Delete')
    ) {
      event.preventDefault();
      handleRequestDelete();
    }
  };

  const actionBar = selectedItem ? (
    <ActionBar
      pinned={selectedItem.pinned}
      pinPending={actions.pinPendingId === selectedItem.eventId}
      deletePending={actions.deletePending}
      feedback={actions.feedback}
      deleteConfirmationOpen={actions.deleteTargetId !== null}
      onPaste={handlePaste}
      onPastePlainText={handlePastePlainText}
      onTogglePin={handleTogglePin}
      onRequestDelete={handleRequestDelete}
      onCancelDelete={actions.cancelDelete}
      onConfirmDelete={actions.confirmDelete}
    />
  ) : null;

  return (
    <main
      className="palette-stage"
      role="application"
      aria-label="Clipboard history"
      onKeyDown={handlePaletteKeyDown}
    >
      <section
        className="palette-shell"
        aria-label="Clipboard history palette"
        inert={modalOpen}
      >
        <PaletteHeader
          query={query}
          selectedId={navigation.selectedId}
          resultCount={actions.visibleItems.length}
          resultsTruncated={actions.visibleItems.length >= HISTORY_PAGE_SIZE}
          refreshing={refreshing}
          searchInputRef={searchInputRef}
          onQueryChange={handleQueryChange}
          onKeyDown={handleSearchKeyDown}
        />
        <PaletteWorkspace
          status={status}
          items={actions.visibleItems}
          selectedId={navigation.selectedId}
          preview={preview.preview}
          previewStatus={preview.status}
          thumbnailUrl={thumbnail.url}
          thumbnailStatus={thumbnail.status}
          linkPreview={link.preview}
          mobilePreviewOpen={mobilePreviewOpen}
          actions={actionBar}
          onSelect={handleSelect}
          onActivate={handleActivate}
          onRevealSource={() => {
            if (navigation.selectedId !== null) void gateway.revealSource(navigation.selectedId);
          }}
          onOpenPreview={() => setMobilePreviewOpen(true)}
          onClosePreview={() => {
            setMobilePreviewOpen(false);
            focusSearch();
          }}
        />
        {/* ActionBar owns the live region while a row is selected; this covers
            the case where the last item was just deleted and it unmounted. */}
        {selectedItem === null && actions.feedback ? (
          <p className="sr-only" role="status" aria-live="polite">
            {actions.feedback}
          </p>
        ) : null}
        <footer className="palette-footer">
          <span>↵ paste · ⌘C copy · ⌘⇧V plain text · ⌘⇧Space summon</span>
          {/* Out of the way but still visible: a shortcut nobody was told about
              is the same as no way in. */}
          <span className="palette-footer__entries">
            <button
              type="button"
              className="footer-action"
              aria-label="Importuj archiwum"
              onClick={() => setWorkspace('import')}
            >
              Import <kbd>⌘I</kbd>
            </button>
            <button
              type="button"
              className="footer-action"
              aria-label="Open settings"
              onClick={openSettings}
            >
              Settings <kbd>⌘,</kbd>
            </button>
          </span>
        </footer>
      </section>
      {workspace === 'import' ? (
        <ImportWizard gateway={gateway} onClose={closeWorkspace} />
      ) : null}
    </main>
  );
};

export const App = ({ gateway }: AppProps): React.JSX.Element => (
  <GatewayProvider gateway={gateway}>
    <ClipboardPalette />
  </GatewayProvider>
);
