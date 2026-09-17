import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { disable, enable, isEnabled } from '@tauri-apps/plugin-autostart';
import { open, save } from '@tauri-apps/plugin-dialog';
import { createContext, createElement, useContext, type ReactNode } from 'react';

import {
  validateAppCatalog,
  validateImportAnalysis,
  validateImportProgress,
  validateStorageStats,
  type AppEntry,
  type AppSettings,
  type ChatMessage,
  type ChatSettings,
  type ChatStreamEvent,
  type ChatTurn,
  type CopyResult,
  type ExportSummary,
  type HistoryPage,
  type KeyvaultSecret,
  type LinkPreview,
  type ImportAnalysis,
  type ImportProgress,
  type ImportRunHandle,
  type Preview,
  type SearchRequest,
  type StorageStats,
  type Thumbnail,
  type KeyvaultIdentity,
  type PairingStarted,
  type PairingStatus,
  type ShortcutStatus,
  type ShortcutRelease,
} from './contracts';
import { mockGateway } from './mockGateway';

export interface ClipboardGateway {
  search(request: SearchRequest): Promise<HistoryPage>;
  preview(eventId: number): Promise<Preview>;
  setPinned(eventId: number, pinned: boolean): Promise<void>;
  deleteEvent(eventId: number): Promise<void>;
  /**
   * Puts an entry back on the clipboard, and when `paste` is set also sends it
   * to the window the user was in before the palette appeared.
   */
  copyEvent(eventId: number, plainText: boolean, paste: boolean): Promise<CopyResult>;
  chooseImportFile(): Promise<string | null>;
  chooseImportDirectory(): Promise<string | null>;
  analyzeImport(path: string, password?: string): Promise<ImportAnalysis>;
  startImport(analysisId: string): Promise<ImportRunHandle>;
  discardImportAnalysis(analysisId: string): Promise<void>;
  getImportStatus(runId: string): Promise<ImportProgress>;
  revealSource(eventId: number): Promise<void>;
  openSettingsWindow(): Promise<void>;
  linkPreview(eventId: number): Promise<LinkPreview | null>;
  onLinkPreviewReady?(listener: (eventId: number) => void): () => void;
  chooseExportDirectory(): Promise<string | null>;
  exportHistory(directory: string): Promise<ExportSummary>;
  /** Lists the vault's secret metadata — nothing sealed, nothing opened. */
  keyvaultList(): Promise<KeyvaultSecret[]>;
  /**
   * Puts one secret on the clipboard. The value itself stays in the core:
   * this resolves knowing only whether it worked.
   */
  keyvaultCopySecret(slug: string): Promise<void>;
  /**
   * Begins pairing with a vault and resolves to the fingerprint to display.
   *
   * The fingerprint is not decoration: it is the only thing tying the page being approved to
   * this application, so the interface must show it and say what it is for.
   */
  keyvaultPairStart(url: string): Promise<PairingStarted>;
  keyvaultPairPoll(): Promise<{ status: PairingStatus }>;
  keyvaultPairCancel(): Promise<void>;
  /** Whether this device is paired and which vault it knows. A file read, not a request. */
  keyvaultIdentity(): Promise<KeyvaultIdentity>;
  /**
   * Forgets this device's pairing so it can pair again.
   *
   * Local only: the device registered in the vault keeps existing, because revoking it needs an
   * account and this application holds a token. The next pairing offers to retire it.
   */
  keyvaultResetPairing(): Promise<void>;
  /**
   * The chat window's commands. Optional the way the shortcut extras are:
   * a gateway without them — a test harness, or a palette that never
   * chats — is still a complete gateway, and the chat window is the one
   * place that requires them.
   */
  chatSend?(messages: ChatMessage[]): Promise<ChatTurn>;
  chatStop?(id: string): Promise<boolean>;
  /** The models the active provider offers, fetched live from its list. */
  chatListModels?(): Promise<string[]>;
  /**
   * Saves one file the chat produced. The native save dialog asks where;
   * resolves false when the user cancelled.
   */
  saveGeneratedFile?(defaultName: string, contents: string): Promise<boolean>;
  /** Copies a code block, through the core like every other copy. */
  copyChatText?(text: string): Promise<void>;
  /** Opens one http(s) link from an answer, in the user's browser. */
  openExternalUrl?(url: string): Promise<void>;
  getChatSettings?(): Promise<ChatSettings>;
  saveChatSettings?(settings: ChatSettings): Promise<ChatSettings>;
  /** Opens the chat window, its own window beside the palette. */
  openChatWindow?(): Promise<void>;
  /**
   * Calls back for every chat stream event: tokens as they arrive, the
   * settle when the turn ends — by answer, refusal or stop.
   */
  onChatEvent?(listener: (event: ChatStreamEvent) => void): () => void;
  /**
   * Calls back whenever the core records something new. Returns a function
   * that stops listening; without it the palette would show a history that is
   * already out of date the moment the user copies anything.
   *
   * Optional: a gateway with no live core behind it — the browser preview —
   * has nothing to report.
   */
  onHistoryChanged?(listener: () => void): () => void;
  /** The menu bar asking this window to open its settings. */
  getThumbnail(eventId: number): Promise<Thumbnail | null>;
  getSettings(): Promise<AppSettings>;
  isAutostartEnabled(): Promise<boolean>;
  setAutostartEnabled(enabled: boolean): Promise<void>;
  saveSettings(settings: AppSettings): Promise<AppSettings>;
  getStorageStats(): Promise<StorageStats>;
  /**
   * The whole launchable catalog at once. The palette filters as the user
   * types, so there is no per-keystroke round trip to design here.
   *
   * When the core already has a catalog, the answer is that copy — the
   * palette never waits — and a background rescan starts beside it.
   */
  listApps(): Promise<AppEntry[]>;
  /**
   * Calls back when a background catalog rescan found something new:
   * an application appeared or vanished since the last answer. The palette
   * refetches `listApps` on it, which returns the fresh copy at once.
   *
   * Optional, like every core signal: a gateway with no live core behind
   * it has nothing to report.
   */
  onAppsChanged?(listener: () => void): () => void;
  /** Starts an application by the catalog path it was listed under. */
  launchApp(path: string): Promise<void>;
  /**
   * The rendered icon for a catalog path, or null when there is nothing to
   * draw — the row keeps its glyph and carries on.
   */
  getAppIcon(path: string): Promise<Thumbnail | null>;
  /**
   * Opens the Accessibility list in System Settings — the one switch that
   * decides whether an entry can be pasted rather than only copied.
   */
  openAccessibilitySettings(): Promise<void>;
  /** What the summoning shortcut is doing, as opposed to what it was asked to do. */
  getShortcutStatus?(): Promise<ShortcutStatus>;
  /**
   * Turns off the system shortcuts standing on the configured chord.
   *
   * Deliberate and user-initiated: this changes a setting that belongs to the
   * whole machine, not to this application.
   */
  freeSummoningShortcut?(): Promise<ShortcutRelease>;
  /** Hands the system back what freeing the shortcut took. */
  restoreSystemShortcut?(): Promise<ShortcutRelease>;
  /** Opens the Keyboard shortcut list, for when the automatic route is refused. */
  openKeyboardSettings?(): Promise<void>;
}

/**
 * Subscribes to one core event.
 *
 * A signal from the core is a convenience: if the event bridge is unavailable
 * the palette must still open, so a failure here degrades to a window that
 * misses a refresh rather than to a broken one.
 */
const subscribe = <T = void,>(
  event: string,
  listener: (payload: T) => void,
): (() => void) => {
  let stop: (() => void) | null = null;
  let cancelled = false;
  void listen<T>(event, (message) => listener(message.payload))
    .then((unlisten) => {
      if (cancelled) unlisten();
      else stop = unlisten;
    })
    .catch(() => undefined);
  return () => {
    cancelled = true;
    stop?.();
  };
};

export const tauriGateway: ClipboardGateway = {
  search: (request) => invoke<HistoryPage>('search_history', { request }),
  preview: (eventId) => invoke<Preview>('get_preview', { eventId }),
  setPinned: (eventId, pinned) => invoke<void>('set_pinned', { eventId, pinned }),
  deleteEvent: (eventId) => invoke<void>('delete_event', { eventId }),
  copyEvent: (eventId, plainText, paste) =>
    invoke<CopyResult>('copy_event', { eventId, plainText, paste }),
  chooseImportFile: () =>
    open({
      title: 'Choose a clipboard history export',
      multiple: false,
      filters: [{ name: 'Clipboard export', extensions: ['json', 'rayconfig'] }],
    }),
  chooseImportDirectory: () =>
    open({
      title: 'Choose the clipboard history export directory',
      directory: true,
      recursive: true,
      multiple: false,
    }),
  analyzeImport: (path, password) =>
    invoke<ImportAnalysis>('analyze_import', { path, password: password ?? null }).then(
      validateImportAnalysis,
    ),
  startImport: (analysisId) => invoke<ImportRunHandle>('start_import', { analysisId }),
  discardImportAnalysis: (analysisId) =>
    invoke<void>('discard_import_analysis', { analysisId }),
  getImportStatus: (runId) =>
    invoke<ImportProgress>('get_import_status', { runId }).then((progress) =>
      validateImportProgress(progress, runId),
    ),
  revealSource: (eventId) => invoke<void>('reveal_source', { eventId }),
  openSettingsWindow: () => invoke<void>('open_settings_window'),
  getShortcutStatus: () => invoke<ShortcutStatus>('get_shortcut_status'),
  freeSummoningShortcut: () => invoke<ShortcutRelease>('free_summoning_shortcut'),
  restoreSystemShortcut: () => invoke<ShortcutRelease>('restore_system_shortcut'),
  openKeyboardSettings: () => invoke<void>('open_keyboard_settings_window'),
  linkPreview: (eventId) => invoke<LinkPreview | null>('get_link_preview', { eventId }),
  onLinkPreviewReady: (listener) => subscribe<number>('link-preview-ready', listener),
  chooseExportDirectory: () =>
    save({
      title: 'Choose a directory for the history export',
      defaultPath: 'clipboard-export',
    }),
  exportHistory: (directory) => invoke<ExportSummary>('export_history', { directory }),
  keyvaultList: () => invoke<KeyvaultSecret[]>('keyvault_list'),
  keyvaultCopySecret: (slug) => invoke<void>('keyvault_copy_secret', { slug }),
  keyvaultPairStart: (url) => invoke<PairingStarted>('keyvault_pair_start', { url }),
  keyvaultPairPoll: () => invoke<{ status: PairingStatus }>('keyvault_pair_poll'),
  keyvaultPairCancel: () => invoke<void>('keyvault_pair_cancel'),
  keyvaultIdentity: () => invoke<KeyvaultIdentity>('keyvault_identity'),
  keyvaultResetPairing: () => invoke<void>('keyvault_reset_pairing'),
  onHistoryChanged: (listener) => subscribe('history-changed', listener),
  getThumbnail: (eventId) => invoke<Thumbnail | null>('get_thumbnail', { eventId }),
  getSettings: () => invoke<AppSettings>('get_settings'),
  isAutostartEnabled: () => isEnabled(),
  setAutostartEnabled: (enabled) => (enabled ? enable() : disable()),
  saveSettings: (settings) => invoke<AppSettings>('save_settings', { settings }),
  getStorageStats: () =>
    invoke<StorageStats>('get_storage_stats').then(validateStorageStats),
  listApps: () => invoke<AppEntry[]>('list_apps').then(validateAppCatalog),
  onAppsChanged: (listener) => subscribe('apps-catalog-changed', listener),
  chatSend: (messages) => invoke<ChatTurn>('chat_send', { messages }),
  chatStop: (id) => invoke<boolean>('chat_stop', { id }),
  chatListModels: () => invoke<string[]>('chat_list_models'),
  saveGeneratedFile: async (defaultName, contents) => {
    const target = await save({
      title: 'Save the file the chat produced',
      defaultPath: defaultName,
    });
    if (target === null) return false;
    await invoke<void>('save_generated_file', { path: target, contents });
    return true;
  },
  copyChatText: (text) => invoke<void>('copy_chat_text', { text }),
  openExternalUrl: (url) => invoke<void>('open_external_url', { url }),
  getChatSettings: () => invoke<ChatSettings>('get_chat_settings'),
  saveChatSettings: (settings) =>
    invoke<ChatSettings>('save_chat_settings', { settings }),
  openChatWindow: () => invoke<void>('open_chat_window'),
  onChatEvent: (listener) => {
    const stopDelta = subscribe<{ id: string; part: string; text: string }>(
      'chat-delta',
      (payload) =>
        listener({
          kind: 'delta',
          id: payload.id,
          part: payload.part === 'reasoning' ? 'reasoning' : 'answer',
          text: payload.text,
        }),
    );
    const stopDone = subscribe<{ id: string }>('chat-done', (payload) =>
      listener({ kind: 'done', id: payload.id }),
    );
    const stopError = subscribe<{ id: string; code?: string }>('chat-error', (payload) =>
      listener({ kind: 'error', id: payload.id, code: payload.code ?? 'chat_failed' }),
    );
    return () => {
      stopDelta();
      stopDone();
      stopError();
    };
  },
  launchApp: (path) => invoke<void>('launch_app', { path }),
  getAppIcon: (path) => invoke<Thumbnail | null>('get_app_icon', { path }),
  openAccessibilitySettings: () => invoke<void>('open_accessibility_settings_window'),
};

interface GatewayProviderProps {
  children: ReactNode;
  gateway?: ClipboardGateway;
}

const GatewayContext = createContext<ClipboardGateway | null>(null);

const isTauriRuntime = (): boolean =>
  typeof window !== 'undefined' && '__TAURI_INTERNALS__' in window;

export const GatewayProvider = ({
  children,
  gateway,
}: GatewayProviderProps): ReactNode => {
  const selectedGateway = gateway ?? (isTauriRuntime() ? tauriGateway : mockGateway);
  return createElement(GatewayContext.Provider, { value: selectedGateway }, children);
};

export const useGateway = (): ClipboardGateway => {
  const gateway = useContext(GatewayContext);
  if (!gateway) {
    throw new Error('gateway_provider_missing');
  }
  return gateway;
};

export { mockGateway };
