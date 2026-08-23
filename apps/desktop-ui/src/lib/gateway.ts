import { invoke } from '@tauri-apps/api/core';
import { createContext, createElement, useContext, type ReactNode } from 'react';

import {
  validateImportProgress,
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
  analyzeImport(path: string): Promise<ImportAnalysis>;
  startImport(analysisId: string): Promise<ImportRunHandle>;
  discardImportAnalysis(analysisId: string): Promise<void>;
  getImportStatus(runId: string): Promise<ImportProgress>;
  getThumbnail(eventId: number): Promise<Thumbnail | null>;
  getSettings(): Promise<AppSettings>;
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
  analyzeImport: (path) => invoke<ImportAnalysis>('analyze_import', { path }),
  startImport: (analysisId) => invoke<ImportRunHandle>('start_import', { analysisId }),
  discardImportAnalysis: (analysisId) =>
    invoke<void>('discard_import_analysis', { analysisId }),
  getImportStatus: (runId) =>
    invoke<ImportProgress>('get_import_status', { runId }).then(validateImportProgress),
  getThumbnail: (eventId) => invoke<Thumbnail | null>('get_thumbnail', { eventId }),
  getSettings: () => invoke<AppSettings>('get_settings'),
  saveSettings: (settings) => invoke<AppSettings>('save_settings', { settings }),
  getStorageStats: () => invoke<StorageStats>('get_storage_stats'),
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
