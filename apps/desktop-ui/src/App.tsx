import {
  useEffect,
  useRef,
  useState,
  type KeyboardEvent as ReactKeyboardEvent,
  type KeyboardEventHandler,
} from 'react';

import { ActionBar } from './components/ActionBar';
import { FilterRail } from './components/FilterRail';
import { PaletteHeader } from './components/PaletteHeader';
import { PaletteWorkspace } from './components/PaletteWorkspace';
import { useHistoryActions } from './hooks/useHistoryActions';
import { useHistorySearch } from './hooks/useHistorySearch';
import { useKeyboardNavigation } from './hooks/useKeyboardNavigation';
import { useSelectedPreview } from './hooks/useSelectedPreview';
import { useThumbnail } from './hooks/useThumbnail';
import {
  GatewayProvider,
  useGateway,
  type ClipboardGateway,
} from './lib/gateway';

interface AppProps {
  gateway?: ClipboardGateway;
}

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
  return ownsTextInput && target.id !== 'history-search';
};

const ClipboardPalette = (): React.JSX.Element => {
  const gateway = useGateway();
  const { query, setQuery, status, items } = useHistorySearch(gateway);
  const searchInputRef = useRef<HTMLInputElement>(null);
  const [mobilePreviewOpen, setMobilePreviewOpen] = useState(false);
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
  const handleActivate = (eventId: number): void => actions.copy(eventId, false);
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
    selectedItem?.kind === 'image' && selectedItem.hasThumbnail && !selectedItem.missingPayload,
  );

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
    if (navigation.selectedId !== null) actions.copy(navigation.selectedId, false);
  };
  const handleCopyPlainText = (): void => {
    if (navigation.selectedId !== null) actions.copy(navigation.selectedId, true);
  };
  const handleTogglePin = (): void => {
    if (selectedItem) actions.togglePin(selectedItem);
  };
  const handleRequestDelete = (): void => {
    if (navigation.selectedId !== null) actions.requestDelete(navigation.selectedId);
  };
  const handlePaletteKeyDown: KeyboardEventHandler<HTMLElement> = (event) => {
    if (
      shortcutIsBlocked(event, actions.deleteTargetId !== null) ||
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
      handleCopyPlainText();
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
      onCopy={handleCopy}
      onCopyPlainText={handleCopyPlainText}
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
      <section className="palette-shell" aria-label="Paleta historii schowka">
        <PaletteHeader
          query={query}
          selectedId={navigation.selectedId}
          resultCount={actions.visibleItems.length}
          searchInputRef={searchInputRef}
          onQueryChange={handleQueryChange}
          onKeyDown={handleSearchKeyDown}
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
          onOpenPreview={() => setMobilePreviewOpen(true)}
          onClosePreview={() => {
            setMobilePreviewOpen(false);
            focusSearch();
          }}
        />
        <p className="sr-only" aria-live="polite">
          {actions.feedback}
        </p>
        <footer className="palette-footer">
          <span>↵ wklej · ⌘C kopiuj · ⌘⇧V zwykły tekst</span>
          <span className="palette-footer__privacy">Tylko na tym urządzeniu</span>
        </footer>
      </section>
    </main>
  );
};

export const App = ({ gateway }: AppProps): React.JSX.Element => (
  <GatewayProvider gateway={gateway}>
    <ClipboardPalette />
  </GatewayProvider>
);
