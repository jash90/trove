import { renderHook } from '@testing-library/react';
import type { ReactNode } from 'react';
import { beforeEach, describe, expect, it, vi } from 'vitest';

import { invoke } from '@tauri-apps/api/core';

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
  });

  it('maps every gateway method to the exact Rust command and camelCase arguments', async () => {
    vi.mocked(invoke).mockResolvedValueOnce({ items: [], nextCursor: null, rankedTruncated: false });
    await tauriGateway.search({ query: 'synthetic', limit: 80, cursor: null });
    await tauriGateway.preview(7);
    await tauriGateway.setPinned(7, true);
    await tauriGateway.deleteEvent(7);
    await tauriGateway.copyEvent(7, true);
    await tauriGateway.analyzeImport('/synthetic/import.json');
    await tauriGateway.startImport('analysis-id');
    await tauriGateway.discardImportAnalysis('analysis-id');
    vi.mocked(invoke).mockResolvedValueOnce(progress);
    await tauriGateway.getImportStatus('run-id');
    await tauriGateway.getThumbnail(7);
    await tauriGateway.getSettings();
    await tauriGateway.saveSettings(settings);
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
      ['get_import_status', { runId: 'run-id' }],
      ['get_thumbnail', { eventId: 7 }],
      ['get_settings'],
      ['save_settings', { settings }],
      ['get_storage_stats'],
    ]);
  });

  it('keeps native and browser gateways at runtime surface parity', () => {
    const gateways: ClipboardGateway[] = [tauriGateway, mockGateway];

    expect(gateways).toHaveLength(2);
    expect(Object.keys(mockGateway).sort()).toEqual(Object.keys(tauriGateway).sort());
  });

  it('rejects an inconsistent import status at the IPC boundary', async () => {
    vi.mocked(invoke).mockResolvedValue({ ...progress, processed: 1 });

    await expect(tauriGateway.getImportStatus('run-id')).rejects.toThrow(
      'invalid_import_progress',
    );
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
