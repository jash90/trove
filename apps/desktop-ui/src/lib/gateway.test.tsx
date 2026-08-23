import { renderHook } from '@testing-library/react';
import type { ReactNode } from 'react';
import { beforeEach, describe, expect, it, vi } from 'vitest';

import { invoke } from '@tauri-apps/api/core';
import { disable, enable, isEnabled } from '@tauri-apps/plugin-autostart';
import { open } from '@tauri-apps/plugin-dialog';

import type { AppSettings, ImportProgress } from './contracts';
import {
  GatewayProvider,
  mockGateway,
  tauriGateway,
  useGateway,
  type ClipboardGateway,
} from './gateway';

vi.mock('@tauri-apps/api/core', () => ({
  invoke: vi.fn(),
}));

vi.mock('@tauri-apps/plugin-dialog', () => ({
  open: vi.fn(),
}));

vi.mock('@tauri-apps/plugin-autostart', () => ({
  disable: vi.fn(),
  enable: vi.fn(),
  isEnabled: vi.fn(),
}));

const settings: AppSettings = {
  schemaVersion: 1,
  hotkey: 'CommandOrControl+Shift+V',
  autostart: false,
  retentionDays: null,
  denylistedApps: [],
};

const progress: ImportProgress = {
  runId: '0198f000-0000-7000-8000-000000000010',
  state: 'running',
  processed: 0,
  total: 1,
  imported: 0,
  alreadyPresent: 0,
  skipped: 0,
  failed: 0,
  errorCode: null,
  summary: null,
};

describe('tauriGateway', () => {
  beforeEach(() => {
    vi.mocked(invoke).mockReset().mockResolvedValue(undefined);
    vi.mocked(open).mockReset().mockResolvedValue(null);
    vi.mocked(isEnabled).mockReset().mockResolvedValue(false);
    vi.mocked(enable).mockReset().mockResolvedValue(undefined);
    vi.mocked(disable).mockReset().mockResolvedValue(undefined);
  });

  it('maps every gateway method to the exact Rust command and camelCase arguments', async () => {
    vi.mocked(invoke).mockResolvedValueOnce({ items: [], nextCursor: null, rankedTruncated: false });
    await tauriGateway.search({ query: 'synthetic', limit: 80, cursor: null });
    await tauriGateway.preview(7);
    await tauriGateway.setPinned(7, true);
    await tauriGateway.deleteEvent(7);
    await tauriGateway.copyEvent(7, true);
    await tauriGateway.chooseImportFile();
    await tauriGateway.chooseImportDirectory();
    vi.mocked(invoke).mockResolvedValueOnce({
      analysisId: 'analysis-id',
      total: 1,
      candidateRecords: 1,
      failed: 0,
    });
    await tauriGateway.analyzeImport('/synthetic/import.json');
    await tauriGateway.startImport('analysis-id');
    await tauriGateway.discardImportAnalysis('analysis-id');
    vi.mocked(invoke).mockResolvedValueOnce(progress);
    await tauriGateway.getImportStatus(progress.runId);
    await tauriGateway.getThumbnail(7);
    await tauriGateway.getSettings();
    await tauriGateway.isAutostartEnabled();
    await tauriGateway.setAutostartEnabled(true);
    await tauriGateway.setAutostartEnabled(false);
    await tauriGateway.saveSettings(settings);
    vi.mocked(invoke).mockResolvedValueOnce({
      contentCount: 1,
      eventCount: 1,
      missingPayloadCount: 0,
      databaseBytes: 1,
      blobBytes: 0,
    });
    await tauriGateway.getStorageStats();

    expect(vi.mocked(invoke).mock.calls).toEqual([
      ['search_history', { request: { query: 'synthetic', limit: 80, cursor: null } }],
      ['get_preview', { eventId: 7 }],
      ['set_pinned', { eventId: 7, pinned: true }],
      ['delete_event', { eventId: 7 }],
      ['copy_event', { eventId: 7, plainText: true }],
      ['analyze_import', { path: '/synthetic/import.json' }],
      ['start_import', { analysisId: 'analysis-id' }],
      ['discard_import_analysis', { analysisId: 'analysis-id' }],
      ['get_import_status', { runId: progress.runId }],
      ['get_thumbnail', { eventId: 7 }],
      ['get_settings'],
      ['save_settings', { settings }],
      ['get_storage_stats'],
    ]);
    expect(vi.mocked(open).mock.calls).toEqual([
      [
        {
          title: 'Wybierz eksport historii schowka',
          multiple: false,
          filters: [{ name: 'Eksport JSON', extensions: ['json'] }],
        },
      ],
      [
        {
          title: 'Wybierz katalog eksportu historii schowka',
          directory: true,
          recursive: true,
          multiple: false,
        },
      ],
    ]);
    expect(isEnabled).toHaveBeenCalledOnce();
    expect(enable).toHaveBeenCalledOnce();
    expect(disable).toHaveBeenCalledOnce();
  });

  it('keeps native and browser gateways at runtime surface parity', () => {
    const gateways: ClipboardGateway[] = [tauriGateway, mockGateway];

    expect(gateways).toHaveLength(2);
    expect(Object.keys(mockGateway).sort()).toEqual(Object.keys(tauriGateway).sort());
  });

  it('rejects an inconsistent import status at the IPC boundary', async () => {
    vi.mocked(invoke).mockResolvedValue({ ...progress, processed: 1 });

    await expect(tauriGateway.getImportStatus(progress.runId)).rejects.toThrow(
      'invalid_import_progress',
    );
  });

  it('rejects a status response for a different run at the IPC boundary', async () => {
    vi.mocked(invoke).mockResolvedValue(progress);

    await expect(tauriGateway.getImportStatus('0198f000-0000-7000-8000-0000000000ff'))
      .rejects.toThrow('invalid_import_progress');
  });

  it('rejects malformed analysis and storage responses at the IPC boundary', async () => {
    vi.mocked(invoke).mockResolvedValueOnce({
      analysisId: 'analysis',
      total: 2,
      candidateRecords: 1,
      failed: 0,
    });
    await expect(tauriGateway.analyzeImport('/private/export.json')).rejects.toThrow(
      'invalid_import_analysis',
    );

    vi.mocked(invoke).mockResolvedValueOnce({
      contentCount: 1,
      eventCount: 1,
      missingPayloadCount: 0,
      databaseBytes: Number.MAX_SAFE_INTEGER,
      blobBytes: 1,
    });
    await expect(tauriGateway.getStorageStats()).rejects.toThrow('invalid_storage_stats');
  });
});

describe('GatewayProvider', () => {
  it('uses the browser-safe mock when no gateway is injected', () => {
    const wrapper = ({ children }: { children: ReactNode }) => (
      <GatewayProvider>{children}</GatewayProvider>
    );

    const { result } = renderHook(() => useGateway(), { wrapper });

    expect(result.current).toBe(mockGateway);
  });

  it('exposes an explicitly injected gateway', () => {
    const wrapper = ({ children }: { children: ReactNode }) => (
      <GatewayProvider gateway={tauriGateway}>{children}</GatewayProvider>
    );

    const { result } = renderHook(() => useGateway(), { wrapper });

    expect(result.current).toBe(tauriGateway);
  });
});
