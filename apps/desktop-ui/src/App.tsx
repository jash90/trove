import {
  useEffect,
  useMemo,
  useRef,
  useState,
  type KeyboardEvent as ReactKeyboardEvent,
  type KeyboardEventHandler,
} from 'react';

import { ActionBar } from './components/ActionBar';
import { ImportWizard } from './components/ImportWizard';
import { PaletteHeader } from './components/PaletteHeader';
import { PaletteWorkspace } from './components/PaletteWorkspace';
import { useAppsCatalog } from './hooks/useAppsCatalog';
import { useVaultCatalog } from './hooks/useVaultCatalog';
import { useHistoryActions } from './hooks/useHistoryActions';
import { useHistorySearch } from './hooks/useHistorySearch';
import { useListNavigation } from './hooks/useListNavigation';
import { useSelectedPreview } from './hooks/useSelectedPreview';
import { useLinkPreview } from './hooks/useLinkPreview';
import { useThumbnail } from './hooks/useThumbnail';
import { HISTORY_PAGE_SIZE } from './lib/contracts';
import { filterApps } from './lib/appSearch';
import { buildPaletteItems, keyOfItem, type PaletteItem } from './lib/paletteItems';
import { filterSecrets } from './lib/vaultSearch';
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
  // The catalog lives beside the history from the first frame: the palette
  // is a launcher the moment it opens, not after a mode is toggled into.
  const catalog = useAppsCatalog(gateway, true);
  const visibleApps = useMemo(() => filterApps(catalog.apps, query), [catalog.apps, query]);
  // Only asked for once someone types. The application catalog is a local scan and may load
  // eagerly; this one reaches someone's vault over the network, and an untouched palette has no
  // business doing that.
  const vault = useVaultCatalog(gateway, query.trim() !== '');
  const visibleSecrets = useMemo(
    () => filterSecrets(vault.secrets, query),
    [vault.secrets, query],
  );
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

  const handleLaunch = (path: string): void => {
    setLaunchError(null);
    // A refusal leaves the palette where it is — the application the user
    // asked for did not start, so there is nothing to make way for.
    void gateway.launchApp(path).catch(() => setLaunchError('The application could not be started.'));
  };

  const paletteItems = useMemo<PaletteItem[]>(
    () => buildPaletteItems(visibleApps, actions.visibleItems, query, visibleSecrets),
    [visibleApps, actions.visibleItems, query, visibleSecrets],
  );

  const handleActivate = (entry: PaletteItem): void => {
    if (entry.kind === 'app') {
      handleLaunch(entry.app.path);
    } else if (entry.kind === 'vault') {
      setLaunchError(null);
      // Through the core, which arms the capture suppression before the write, so the key does
      // not land in the history this application exists to keep. The value never comes back
      // here — the interface learns only whether it worked.
      void gateway
        .keyvaultCopySecret(entry.secret.slug)
        .catch(() => setLaunchError('That secret could not be copied from the vault.'));
    } else {
      actions.copy(entry.item.eventId, 'paste');
    }
  };

  const navigation = useListNavigation({
    items: paletteItems,
    keyOf: keyOfItem,
    onActivate: handleActivate,
    // Escape only ever clears the query: with one shared field there is no
    // mode to back out of, and hiding the palette is the shortcut's job.
    onEscape: () => setQuery(''),
  });

  const selected = paletteItems.find((entry) => keyOfItem(entry) === navigation.selectedKey) ?? null;
  const selectedHistoryItem = selected?.kind === 'history' ? selected.item : null;
  const selectedHistoryId = selectedHistoryItem?.eventId ?? null;
  const selectedIndex = selected === null ? -1 : paletteItems.indexOf(selected);
  const activeDescendant =
    selected === null
      ? undefined
      : selected.kind === 'app'
        ? `app-option-${selectedIndex}`
        : selected.kind === 'vault'
          ? `vault-option-${selectedIndex}`
          : `history-option-${selectedHistoryId}`;

  const preview = useSelectedPreview(gateway, selectedHistoryId);
  const thumbnail = useThumbnail(
    gateway,
    selectedHistoryId,
    // Not gated on `hasThumbnail`: that flag is false exactly while no
    // thumbnail exists, which is when one needs rendering. The command
    // answers cheaply when there is no image to render from.
    selectedHistoryItem?.kind === 'image',
  );
  const link = useLinkPreview(gateway, selectedHistoryId, selectedHistoryItem?.kind === 'link');

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

  const handleSelectItem = (entry: PaletteItem): void => {
    navigation.setSelectedKey(keyOfItem(entry));
    // A refusal to launch is about the row that refused, so moving off it
    // clears the alert whichever kind of row the user moved to — the history
    // is one list with the applications now, not a place the launcher's
    // error can follow the user into.
    setLaunchError(null);
    if (entry.kind !== 'app') actions.clearFeedback();
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
    if (selectedHistoryId !== null) actions.copy(selectedHistoryId, 'copy');
  };
  const handlePaste = (): void => {
    if (selectedHistoryId !== null) actions.copy(selectedHistoryId, 'paste');
  };
  const handlePastePlainText = (): void => {
    if (selectedHistoryId !== null) actions.copy(selectedHistoryId, 'pastePlain');
  };
  const handleTogglePin = (): void => {
    if (selectedHistoryItem) actions.togglePin(selectedHistoryItem);
  };
  const handleRequestDelete = (): void => {
    if (selectedHistoryId !== null) actions.requestDelete(selectedHistoryId);
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
    // The row shortcuts below act on a history entry; while an application
    // holds the selection there is none, and ⌘P or Delete doing nothing is
    // the honest behavior. Tab is deliberately absent: focus movement is
    // the browser's to manage, and this palette has no mode to toggle.
    if (selectedHistoryItem === null) return;
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

  const actionBar = selectedHistoryItem ? (
    <ActionBar
      pinned={selectedHistoryItem.pinned}
      pinPending={actions.pinPendingId === selectedHistoryItem.eventId}
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
      aria-label="Clipboard palette"
      onKeyDown={handlePaletteKeyDown}
    >
      <section
        className="palette-shell"
        aria-label="Clipboard history palette"
        inert={modalOpen}
      >
        <PaletteHeader
          query={query}
          activeDescendant={activeDescendant}
          resultCount={visibleSecrets.length + visibleApps.length + actions.visibleItems.length}
          resultsTruncated={actions.visibleItems.length >= HISTORY_PAGE_SIZE}
          refreshing={refreshing}
          searchInputRef={searchInputRef}
          onQueryChange={handleQueryChange}
          onKeyDown={handleSearchKeyDown}
        />
        <PaletteWorkspace
          appsStatus={catalog.status}
          status={status}
          items={paletteItems}
          selectedKey={navigation.selectedKey}
          launchError={launchError}
          preview={preview.preview}
          previewStatus={preview.status}
          thumbnailUrl={thumbnail.url}
          thumbnailStatus={thumbnail.status}
          linkPreview={link.preview}
          selectedItem={selectedHistoryItem}
          mobilePreviewOpen={mobilePreviewOpen}
          actions={actionBar}
          onSelect={handleSelectItem}
          onActivate={handleActivate}
          onRevealSource={() => {
            if (selectedHistoryId !== null) void gateway.revealSource(selectedHistoryId);
          }}
          onOpenPreview={() => setMobilePreviewOpen(true)}
          onClosePreview={() => {
            setMobilePreviewOpen(false);
            focusSearch();
          }}
        />
        {/* ActionBar owns the live region while a row is selected; this covers
            the case where the last item was just deleted and it unmounted. */}
        {selectedHistoryItem === null && actions.feedback ? (
          <p className="sr-only" role="status" aria-live="polite">
            {actions.feedback}
          </p>
        ) : null}
        <footer className="palette-footer">
          <span>↵ open · ⌘C copy · ⌘⇧V plain · ⌘⇧Space summon</span>
          {/* Out of the way but still visible: a shortcut nobody was told about
              is the same as no way in. */}
          <span className="palette-footer__entries">
            <button
              type="button"
              className="footer-action"
              aria-label="Import an archive"
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
