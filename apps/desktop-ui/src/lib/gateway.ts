import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { disable, enable, isEnabled } from '@tauri-apps/plugin-autostart';
import { open } from '@tauri-apps/plugin-dialog';
import { createContext, createElement, useContext, type ReactNode } from 'react';

import {
  validateImportAnalysis,
  validateImportProgress,
  validateStorageStats,
  type AppSettings,
  type CopyResult,
  type HistoryPage,
  type ImportAnalysis,
  type ImportProgress,
  type ImportRunHandle,
  type Preview,
  type SearchRequest,
  type StorageStats,
  type Thumbnail,
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
  onOpenSettingsRequested?(listener: () => void): () => void;
  getThumbnail(eventId: number): Promise<Thumbnail | null>;
  getSettings(): Promise<AppSettings>;
  isAutostartEnabled(): Promise<boolean>;
  setAutostartEnabled(enabled: boolean): Promise<void>;
  saveSettings(settings: AppSettings): Promise<AppSettings>;
  getStorageStats(): Promise<StorageStats>;
}

/**
 * Subscribes to one core event.
 *
 * A signal from the core is a convenience: if the event bridge is unavailable
 * the palette must still open, so a failure here degrades to a window that
 * misses a refresh rather than to a broken one.
 */
const subscribe = (event: string, listener: () => void): (() => void) => {
  let stop: (() => void) | null = null;
  let cancelled = false;
  void listen(event, () => listener())
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
      title: 'Wybierz eksport historii schowka',
      multiple: false,
      filters: [{ name: 'Eksport schowka', extensions: ['json', 'rayconfig'] }],
    }),
  chooseImportDirectory: () =>
    open({
      title: 'Wybierz katalog eksportu historii schowka',
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
  onOpenSettingsRequested: (listener) => subscribe('open-settings', listener),
  onHistoryChanged: (listener) => subscribe('history-changed', listener),
  getThumbnail: (eventId) => invoke<Thumbnail | null>('get_thumbnail', { eventId }),
  getSettings: () => invoke<AppSettings>('get_settings'),
  isAutostartEnabled: () => isEnabled(),
  setAutostartEnabled: (enabled) => (enabled ? enable() : disable()),
  saveSettings: (settings) => invoke<AppSettings>('save_settings', { settings }),
  getStorageStats: () =>
    invoke<StorageStats>('get_storage_stats').then(validateStorageStats),
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
