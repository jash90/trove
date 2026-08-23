import { invoke } from '@tauri-apps/api/core';
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
  copyEvent(eventId: number, plainText: boolean): Promise<CopyResult>;
  chooseImportFile(): Promise<string | null>;
  chooseImportDirectory(): Promise<string | null>;
  analyzeImport(path: string): Promise<ImportAnalysis>;
  startImport(analysisId: string): Promise<ImportRunHandle>;
  discardImportAnalysis(analysisId: string): Promise<void>;
  getImportStatus(runId: string): Promise<ImportProgress>;
  getThumbnail(eventId: number): Promise<Thumbnail | null>;
  getSettings(): Promise<AppSettings>;
  isAutostartEnabled(): Promise<boolean>;
  setAutostartEnabled(enabled: boolean): Promise<void>;
  saveSettings(settings: AppSettings): Promise<AppSettings>;
  getStorageStats(): Promise<StorageStats>;
}

export const tauriGateway: ClipboardGateway = {
  search: (request) => invoke<HistoryPage>('search_history', { request }),
  preview: (eventId) => invoke<Preview>('get_preview', { eventId }),
  setPinned: (eventId, pinned) => invoke<void>('set_pinned', { eventId, pinned }),
  deleteEvent: (eventId) => invoke<void>('delete_event', { eventId }),
  copyEvent: (eventId, plainText) =>
    invoke<CopyResult>('copy_event', { eventId, plainText }),
  chooseImportFile: () =>
    open({
      title: 'Wybierz eksport historii schowka',
      multiple: false,
      filters: [{ name: 'Eksport JSON', extensions: ['json'] }],
    }),
  chooseImportDirectory: () =>
    open({
      title: 'Wybierz katalog eksportu historii schowka',
      directory: true,
      recursive: true,
      multiple: false,
    }),
  analyzeImport: (path) =>
    invoke<ImportAnalysis>('analyze_import', { path }).then(validateImportAnalysis),
  startImport: (analysisId) => invoke<ImportRunHandle>('start_import', { analysisId }),
  discardImportAnalysis: (analysisId) =>
    invoke<void>('discard_import_analysis', { analysisId }),
  getImportStatus: (runId) =>
    invoke<ImportProgress>('get_import_status', { runId }).then((progress) =>
      validateImportProgress(progress, runId),
    ),
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
