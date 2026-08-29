import {
  useEffect,
  useMemo,
  useRef,
  useState,
  type KeyboardEvent as ReactKeyboardEvent,
  type KeyboardEventHandler,
} from 'react';

import { ActionBar } from './components/ActionBar';
import { AppsWorkspace } from './components/AppsWorkspace';
import { ImportWizard } from './components/ImportWizard';
import { PaletteHeader, type PaletteMode } from './components/PaletteHeader';
import { PaletteWorkspace } from './components/PaletteWorkspace';
import { useAppsCatalog } from './hooks/useAppsCatalog';
import { useHistoryActions } from './hooks/useHistoryActions';
import { useHistorySearch } from './hooks/useHistorySearch';
import { useKeyboardNavigation } from './hooks/useKeyboardNavigation';
import { useListNavigation } from './hooks/useListNavigation';
import { useSelectedPreview } from './hooks/useSelectedPreview';
import { useLinkPreview } from './hooks/useLinkPreview';
import { useThumbnail } from './hooks/useThumbnail';
import { HISTORY_PAGE_SIZE } from './lib/contracts';
import { filterApps } from './lib/appSearch';
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
  // One palette, two lists: the history this application exists for, and the
  // installed applications it can also start. Tab moves between them because
  // it touches nothing else — not the query, not any command's modifiers.
  const [mode, setMode] = useState<PaletteMode>('history');
  const catalog = useAppsCatalog(gateway, mode === 'apps');
  const visibleApps = useMemo(() => filterApps(catalog.apps, query), [catalog.apps, query]);
  const [launchError, setLaunchError] = useState<string | null>(null);
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
  const handleLaunch = (path: string): void => {
    setLaunchError(null);
    // A refusal leaves the palette where it is — the application the user
    // asked for did not start, so there is nothing to make way for.
    void gateway.launchApp(path).catch(() => setLaunchError('The application could not be started.'));
  };
  const appsNavigation = useListNavigation({
    items: visibleApps,
    keyOf: (app) => app.path,
    onActivate: (app) => handleLaunch(app.path),
    // Escape climbs down before it leaves: first the query, then the mode,
    // and never the palette itself — only the shortcut and the window's close
    // box ever hide this window.
    onEscape: () => {
      if (query !== '') {
        setQuery('');
      } else {
        setMode('history');
      }
    },
  });
  const appsSelectedIndex = visibleApps.findIndex(
    (app) => app.path === appsNavigation.selectedKey,
  );
  const appsActiveDescendant =
    appsSelectedIndex >= 0 ? `app-option-${appsSelectedIndex}` : undefined;

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
  const handleAppSelect = (path: string): void => {
    appsNavigation.setSelectedKey(path);
    setLaunchError(null);
    focusSearch();
  };
  const handleQueryChange = (nextQuery: string): void => {
    setQuery(nextQuery);
    focusSearch();
  };
  const handleSearchKeyDown: KeyboardEventHandler<HTMLInputElement> = (event) => {
    // Tab is the palette's, not the list's: it never reaches a navigation
    // handler, which would only pass it through unprevented anyway.
    if (event.key === 'Tab' || actions.deleteTargetId !== null) return;
    if (mode === 'apps') {
      appsNavigation.handleKeyDown(event);
    } else {
      navigation.handleKeyDown(event);
    }
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
    // One place handles Tab for the whole palette — the field's handler
    // passes it through — and it must preventDefault, or the browser moves
    // focus instead of the mode. Shift+Tab stays real focus movement, and
    // Alt+Tab belongs to the operating system.
    if (!primaryModifier && !event.shiftKey && !event.altKey && event.key === 'Tab') {
      event.preventDefault();
      setLaunchError(null);
      setMode((current) => (current === 'history' ? 'apps' : 'history'));
      return;
    }
    // The row shortcuts below act on a history entry; in the launcher mode
    // there is none, and ⌘P or Delete doing nothing is the honest behavior.
    if (mode === 'apps' || navigation.selectedId === null) return;
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

  const inAppsMode = mode === 'apps';

  return (
    <main
      className="palette-stage"
      role="application"
      aria-label={inAppsMode ? 'Application launcher' : 'Clipboard history'}
      onKeyDown={handlePaletteKeyDown}
    >
      <section
        className="palette-shell"
        aria-label={inAppsMode ? 'Application launcher palette' : 'Clipboard history palette'}
        inert={modalOpen}
      >
        <PaletteHeader
          mode={mode}
          query={query}
          selectedId={navigation.selectedId}
          resultCount={inAppsMode ? visibleApps.length : actions.visibleItems.length}
          resultsTruncated={!inAppsMode && actions.visibleItems.length >= HISTORY_PAGE_SIZE}
          refreshing={!inAppsMode && refreshing}
          appsActiveDescendant={appsActiveDescendant}
          searchInputRef={searchInputRef}
          onQueryChange={handleQueryChange}
          onKeyDown={handleSearchKeyDown}
        />
        {inAppsMode ? (
          <AppsWorkspace
            status={catalog.status}
            apps={visibleApps}
            selectedKey={appsNavigation.selectedKey}
            launchError={launchError}
            onSelect={handleAppSelect}
            onActivate={handleLaunch}
          />
        ) : (
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
        )}
        {/* ActionBar owns the live region while a row is selected; this covers
            the case where the last item was just deleted and it unmounted. */}
        {selectedItem === null && actions.feedback ? (
          <p className="sr-only" role="status" aria-live="polite">
            {actions.feedback}
          </p>
        ) : null}
        <footer className="palette-footer">
          <span>
            {inAppsMode
              ? '↵ launch · Tab history · ⌘⇧Space summon'
              : '↵ paste · ⌘C copy · ⌘⇧V plain · Tab apps · ⌘⇧Space summon'}
          </span>
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
