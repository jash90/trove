import {
  useEffect,
  useRef,
  useState,
  type KeyboardEvent as ReactKeyboardEvent,
  type KeyboardEventHandler,
} from 'react';

import { ActionBar } from './components/ActionBar';
import { ImportWizard } from './components/ImportWizard';
import { SettingsPanel } from './components/SettingsPanel';
import { FilterRail } from './components/FilterRail';
import { PaletteHeader } from './components/PaletteHeader';
import { PaletteWorkspace } from './components/PaletteWorkspace';
import { useHistoryActions } from './hooks/useHistoryActions';
import { useHistorySearch } from './hooks/useHistorySearch';
import { useKeyboardNavigation } from './hooks/useKeyboardNavigation';
import { useSelectedPreview } from './hooks/useSelectedPreview';
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
  const { query, setQuery, status, items } = useHistorySearch(gateway);
  const searchInputRef = useRef<HTMLInputElement>(null);
  const [mobilePreviewOpen, setMobilePreviewOpen] = useState(false);
  const [workspace, setWorkspace] = useState<'none' | 'import' | 'settings'>('none');
  const importButtonRef = useRef<HTMLButtonElement>(null);
  const settingsButtonRef = useRef<HTMLButtonElement>(null);
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
    selectedItem?.kind === 'image' && selectedItem.hasThumbnail,
  );

  const modalOpen = actions.deleteTargetId !== null || workspace !== 'none';

  // Only one workspace at a time, and the invoker gets focus back so keyboard
  // users are not dropped at the top of the document when a dialog closes.
  const closeWorkspace = (): void => {
    const invoker = workspace === 'import' ? importButtonRef : settingsButtonRef;
    setWorkspace('none');
    queueMicrotask(() => invoker.current?.focus());
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
    if (
      shortcutIsBlocked(event, modalOpen) ||
      navigation.selectedId === null
    ) {
      return;
    }
    const key = event.key.toLocaleLowerCase('en-US');
    const primaryModifier = event.metaKey || event.ctrlKey;
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
      aria-label="Historia schowka"
      onKeyDown={handlePaletteKeyDown}
    >
      <section
        className="palette-shell"
        aria-label="Paleta historii schowka"
        inert={modalOpen}
      >
        <PaletteHeader
          query={query}
          selectedId={navigation.selectedId}
          resultCount={actions.visibleItems.length}
          resultsTruncated={actions.visibleItems.length >= HISTORY_PAGE_SIZE}
          searchInputRef={searchInputRef}
          importButtonRef={importButtonRef}
          settingsButtonRef={settingsButtonRef}
          onQueryChange={handleQueryChange}
          onKeyDown={handleSearchKeyDown}
          onOpenImport={() => setWorkspace('import')}
          onOpenSettings={() => setWorkspace('settings')}
        />
        <FilterRail query={query} onQueryChange={handleQueryChange} />
        <PaletteWorkspace
          status={status}
          items={actions.visibleItems}
          selectedId={navigation.selectedId}
          preview={preview.preview}
          previewStatus={preview.status}
          thumbnailUrl={thumbnail.url}
          thumbnailStatus={thumbnail.status}
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
          <span>↵ wklej · ⌘C kopiuj · ⌘⇧V zwykły tekst · ⌘⇧Space przywołaj</span>
          <span className="palette-footer__privacy">Tylko na tym urządzeniu</span>
        </footer>
      </section>
      {workspace === 'import' ? (
        <ImportWizard gateway={gateway} onClose={closeWorkspace} />
      ) : null}
      {workspace === 'settings' ? (
        <SettingsPanel gateway={gateway} onClose={closeWorkspace} />
      ) : null}
    </main>
  );
};

export const App = ({ gateway }: AppProps): React.JSX.Element => (
  <GatewayProvider gateway={gateway}>
    <ClipboardPalette />
  </GatewayProvider>
);
