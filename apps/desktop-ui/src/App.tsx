import { getCurrentWindow } from '@tauri-apps/api/window';
import {
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
  type KeyboardEvent as ReactKeyboardEvent,
  type KeyboardEventHandler,
} from 'react';

import { ActionBar } from './components/ActionBar';
import { ImportWizard } from './components/ImportWizard';
import {
  PALETTE_CATEGORIES,
  PALETTE_CHAT_CATEGORY,
  PaletteHeader,
  type PaletteMode,
  type PaletteView,
} from './components/PaletteHeader';
import { acceleratorFromKeyEvent } from './components/SettingsPanel';
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

/**
 * The shortcut written the way a keyboard is drawn rather than the way it is stored.
 *
 * The footer used to name it in prose, hardcoded, so it went on advertising
 * ⌘⇧Space after the shortcut changed — and a shortcut nobody was told about
 * correctly is worse than one nobody was told about at all.
 */
export const shortcutHint = (hotkey: string | null): string => {
  if (hotkey === null) return '';
  return hotkey
    .split('+')
    .map((part) => {
      switch (part) {
        case 'CommandOrControl':
        case 'Command':
          return '⌘';
        case 'Control':
          return '⌃';
        case 'Alt':
          return '⌥';
        case 'Shift':
          return '⇧';
        default:
          return part;
      }
    })
    .join('');
};

/**
 * Whether a keypress is the summoning shortcut arriving inside the palette.
 *
 * The shortcut is registered with the system and toggles the window from
 * outside, but pressing it while the search field has focus did nothing: the
 * field is a text field, and the chord went into it rather than through it. The
 * shortcut is meant to be in charge of both directions — that is the whole
 * reason there is no second key for putting the palette away — so the palette
 * answers it itself when it is the one holding focus.
 *
 * Compared against the accelerator the settings screen stores, so a rebound
 * shortcut closes the palette exactly as the built-in one does.
 */
export const isSummoningShortcut = (
  event: { code: string; metaKey: boolean; ctrlKey: boolean; altKey: boolean; shiftKey: boolean },
  hotkey: string | null,
): boolean => {
  if (hotkey === null) return false;
  const pressed = acceleratorFromKeyEvent(event);
  return pressed !== null && pressed === hotkey;
};

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
  // The palette opens on its categories — Applications, Clipboard history,
  // Key vault — and one of them is then a deliberate pick away. Which list
  // someone came for is a fact about them; the home view asks instead of
  // guessing. The setting can turn the whole arrangement off: `view` is
  // then the combined list, exactly as the palette used to be.
  const [mode, setMode] = useState<PaletteMode | 'home'>('home');
  // Whether the palette has modes at all — a setting, read below and
  // refreshed each time the palette comes back to the front.
  const [paletteModes, setPaletteModes] = useState(true);
  const view: PaletteView = paletteModes ? mode : 'all';
  // The catalog is loaded whichever mode is showing it, so that Tab is a
  // filter change and never a wait.
  const catalog = useAppsCatalog(gateway, true);
  const visibleApps = useMemo(
    () => (view === 'apps' || view === 'all' ? filterApps(catalog.apps, query) : []),
    [view, catalog.apps, query],
  );
  // Only asked for once someone types. The application catalog is a local scan and may load
  // eagerly; this one reaches someone's vault over the network, and an untouched palette has no
  // business doing that.
  // The vault is asked for the moment its category is entered — a
  // deliberate act, like typing was before it — or, combined, when someone
  // types. Never on an untouched palette: the ask crosses the network.
  const vault = useVaultCatalog(gateway, view === 'vault' || (view === 'all' && query.trim() !== ''));
  const visibleSecrets = useMemo(() => {
    if (view === 'vault') {
      // Browsed, not only searched: the whole key list is the point of the
      // category, so an empty query shows everything the vault returned.
      return query.trim() === '' ? vault.secrets : filterSecrets(vault.secrets, query);
    }
    if (view === 'all' && query.trim() !== '') return filterSecrets(vault.secrets, query);
    return [];
  }, [view, vault.secrets, query]);
  const [launchError, setLaunchError] = useState<string | null>(null);
  // The shortcut the palette has to recognise when it is pressed inside the
  // window rather than outside it, and the setting that decides whether the
  // palette has modes at all. Re-read on focus: settings change in their own
  // window, and the palette notices the next time it is in front.
  const [summoningShortcut, setSummoningShortcut] = useState<string | null>(null);
  const refreshSettings = useCallback((): void => {
    // Defensive on the call as well as the promise: the palette has to open
    // whether or not the settings can be read, and a shortcut it does not know
    // costs a way of closing, not the window.
    try {
      void gateway
        .getSettings()
        .then((settings) => {
          setSummoningShortcut(settings.hotkey);
          setPaletteModes(settings.paletteModes);
        })
        .catch(() => undefined);
    } catch {
      /* no settings to read; the system shortcut still toggles the window */
    }
  }, [gateway]);
  useEffect(() => {
    refreshSettings();
  }, [refreshSettings]);
  const focusSearch = (): void => searchInputRef.current?.focus();
  useEffect(() => {
    searchInputRef.current?.focus();
  }, []);
  useEffect(() => {
    // The palette is hidden and shown, never destroyed, so the effect above runs once and never
    // again — the second time it was summoned the caret was nowhere. Focus follows the window
    // gaining focus instead, which covers all three ways it is summoned (the shortcut, the menu
    // bar icon, and Show in its menu) without a new event to keep in step with them.
    //
    // Coming back by Cmd-Tab focuses the field too. That is right for this window: it is one you
    // type into.
    let stop: (() => void) | null = null;
    let cancelled = false;
    // try/catch around the call itself, not only the promise: outside a Tauri window
    // getCurrentWindow throws where it stands, and a .catch() never gets the chance.
    try {
      void getCurrentWindow()
        .onFocusChanged(({ payload: focused }) => {
          if (focused) {
            focusSearch();
            // Settings may have changed in their own window while this one
            // was out of front; the next summoning picks them up.
            refreshSettings();
          }
        })
        .then((unlisten) => {
          if (cancelled) unlisten();
          else stop = unlisten;
        })
        .catch(() => undefined);
    } catch {
      /* no window to listen to; the palette works without this */
    }
    return () => {
      cancelled = true;
      stop?.();
    };
  }, []);
  const actions = useHistoryActions({
    gateway,
    items,
    onFocusSearch: focusSearch,
    onOpenPreview: () => setMobilePreviewOpen(true),
  });
  // The history rows the palette shows: every one of them while the
  // history side is up — the view decides, the source is always the same
  // list.
  const historyItems = view === 'history' || view === 'all' ? actions.visibleItems : [];

  const handleLaunch = (path: string): void => {
    setLaunchError(null);
    // A refusal leaves the palette where it is — the application the user
    // asked for did not start, so there is nothing to make way for.
    void gateway.launchApp(path).catch(() => setLaunchError('The application could not be started.'));
  };

  const paletteItems = useMemo<PaletteItem[]>(
    () => buildPaletteItems(visibleApps, historyItems, query, visibleSecrets),
    [visibleApps, historyItems, query, visibleSecrets],
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
    // A launch and a key copy both end with the palette going away — the
    // next summoning opens back on the categories, not on whichever side
    // the last errand ran on. A history paste is different: it may leave
    // the palette standing with something to say (the Accessibility
    // refusal and its fix), and that answer belongs to the row and the
    // selection still on screen. With categories off there is nothing to
    // reset.
    if (paletteModes && entry.kind !== 'history') setMode('home');
  };

  const navigation = useListNavigation({
    items: paletteItems,
    keyOf: keyOfItem,
    onActivate: handleActivate,
    // Escape backs out one step at a time: first the query, then the
    // category — back to the chooser — and only hiding the palette remains
    // the global shortcut's job. Home has nothing above it to back out to.
    onEscape: () => {
      if (query !== '') {
        setQuery('');
      } else if (paletteModes && mode !== 'home') {
        setMode('home');
      }
    },
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
    // Typing from home means the history: the palette is a clipboard
    // manager before it is anything else, and a query is the one answer
    // that needs no category picked. Picking a tile first keeps any other
    // meaning.
    if (paletteModes && mode === 'home' && nextQuery.trim() !== '') {
      setMode('history');
    }
    setQuery(nextQuery);
    focusSearch();
  };
  const handleSearchKeyDown: KeyboardEventHandler<HTMLInputElement> = (event) => {
    // The digits pick a category straight from home, in tile order —
    // the keys the tiles themselves display. The fourth opens the chat
    // window, a destination beside the palette rather than a view of it.
    if (
      paletteModes &&
      mode === 'home' &&
      ['1', '2', '3', '4'].includes(event.key) &&
      !event.metaKey &&
      !event.ctrlKey &&
      !event.altKey
    ) {
      if (event.key === PALETTE_CHAT_CATEGORY.key) {
        event.preventDefault();
        void gateway.openChatWindow?.().catch(() => undefined);
        return;
      }
      const category = PALETTE_CATEGORIES.find((entry) => entry.key === event.key);
      if (category) {
        event.preventDefault();
        setMode(category.mode);
        return;
      }
    }
    // Tab, from the field only, cycles the categories: home, then each of
    // them in tile order, then home again. Shift+Tab keeps the browser's
    // meaning — a keyboard user's route out of the field to the controls
    // below must survive this. With categories off, plain Tab keeps the
    // browser's meaning too: there is nothing to cycle.
    if (
      paletteModes &&
      event.key === 'Tab' &&
      !event.shiftKey &&
      !event.metaKey &&
      !event.ctrlKey &&
      !event.altKey
    ) {
      event.preventDefault();
      setMode((previous) => {
        const order: Array<PaletteMode | 'home'> = ['home', ...PALETTE_CATEGORIES.map((c) => c.mode)];
        const index = order.indexOf(previous);
        const next = order[(index + 1) % order.length];
        return next === undefined ? 'home' : next;
      });
      return;
    }
    if (actions.deleteTargetId === null) navigation.handleKeyDown(event);
  };
  // Hidden rather than closed, the same as every other way the palette goes
  // away: closing it would end the process and take the history recording with
  // it. Outside a Tauri window there is nothing to hide, and the palette works
  // in a browser without one.
  const hideWindow = (): void => {
    try {
      void getCurrentWindow().hide().catch(() => undefined);
    } catch {
      /* no window to hide */
    }
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
  // A refusal to open the settings pane is not worth a second error next to the
  // first one: the message already names the permission and where it lives.
  const handleGrantPastePermission = (): void => {
    void gateway.openAccessibilitySettings().catch(() => undefined);
  };
  const handlePaletteKeyDown: KeyboardEventHandler<HTMLElement> = (event) => {
    // Checked before anything else, including the text-field guard: this is the
    // one chord that must work wherever focus happens to be, because it is the
    // only way the palette is put away without reaching for the mouse.
    if (isSummoningShortcut(event, summoningShortcut)) {
      event.preventDefault();
      hideWindow();
      return;
    }
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
    // The chat window is one chord away, like the importer and settings:
    // a conversation is its own window, not a mode of this one.
    if (primaryModifier && !event.shiftKey && key === 'k') {
      event.preventDefault();
      void gateway.openChatWindow?.().catch(() => undefined);
      return;
    }
    // ⌘4 opens the chat window whatever the palette is showing: chat is a
    // destination, not a category, and a palette with the categories turned
    // off must not lose it.
    if (primaryModifier && !event.shiftKey && key === PALETTE_CHAT_CATEGORY.key) {
      event.preventDefault();
      void gateway.openChatWindow?.().catch(() => undefined);
      return;
    }
    // Direct category picks, in tile order: ⌘1 applications, ⌘2 history,
    // ⌘3 vault. Meaningless with the categories off, so they wait for them.
    if (
      paletteModes &&
      primaryModifier &&
      !event.shiftKey &&
      ['1', '2', '3'].includes(key)
    ) {
      const category = PALETTE_CATEGORIES.find((entry) => entry.key === key);
      if (category) {
        event.preventDefault();
        setMode(category.mode);
        return;
      }
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
      feedbackMode={actions.feedbackMode}
      deleteConfirmationOpen={actions.deleteTargetId !== null}
      onPaste={handlePaste}
      onPastePlainText={handlePastePlainText}
      onTogglePin={handleTogglePin}
      onRequestDelete={handleRequestDelete}
      onCancelDelete={actions.cancelDelete}
      onConfirmDelete={actions.confirmDelete}
      onGrantPastePermission={handleGrantPastePermission}
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
        aria-label="Trove palette"
        inert={modalOpen}
      >
        <PaletteHeader
          query={query}
          mode={view}
          activeDescendant={activeDescendant}
          resultCount={visibleSecrets.length + visibleApps.length + historyItems.length}
          resultsTruncated={
            (view === 'history' || view === 'all') && historyItems.length >= HISTORY_PAGE_SIZE
          }
          refreshing={refreshing}
          searchInputRef={searchInputRef}
          onQueryChange={handleQueryChange}
          onKeyDown={handleSearchKeyDown}
        />
        <PaletteWorkspace
          mode={view}
          onPickCategory={setMode}
          onOpenChat={() => void gateway.openChatWindow?.().catch(() => undefined)}
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
          <span>
            ↵ open · ⌘C copy · ⌘⇧V plain{paletteModes ? ' · ⇥ mode' : ''}
            {summoningShortcut === null ? '' : ` · ${shortcutHint(summoningShortcut)} summon`}
          </span>
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
              aria-label="Open the chat window"
              onClick={() => void gateway.openChatWindow?.().catch(() => undefined)}
            >
              Chat <kbd>⌘K</kbd>
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
