import type { ClipboardGateway } from './gateway';
import {
  SYNTHETIC_APPS,
  SYNTHETIC_APP_ICON,
  SYNTHETIC_HISTORY_ITEMS,
  SYNTHETIC_IMPORT_PROGRESS,
  SYNTHETIC_KEYVAULT_SECRETS,
  SYNTHETIC_SETTINGS,
  SYNTHETIC_STORAGE_STATS,
} from './fixtures';

let historyItems = SYNTHETIC_HISTORY_ITEMS.map((item) => ({ ...item }));
let settings = {
  ...SYNTHETIC_SETTINGS,
  denylistedApps: [...SYNTHETIC_SETTINGS.denylistedApps],
  keyvault: { ...SYNTHETIC_SETTINGS.keyvault },
};
let autostartEnabled = SYNTHETIC_SETTINGS.autostart;

export const mockGateway: ClipboardGateway = {
  search: async (request) => {
    const normalizedQuery = request.query.trim().toLocaleLowerCase('en-US');
    const matches = normalizedQuery
      ? historyItems.filter((item) =>
          `${item.preview} ${item.sourceAppName ?? ''}`.toLocaleLowerCase('en-US').includes(
            normalizedQuery,
          ),
        )
      : historyItems;
    return {
      items: matches.slice(0, request.limit).map((item) => ({ ...item })),
      nextCursor: null,
      rankedTruncated: false,
    };
  },
  preview: async (eventId) => {
    const item = historyItems.find((candidate) => candidate.eventId === eventId);
    if (!item) throw new Error('history_event_not_found');
    return {
      eventId: item.eventId,
      kind: item.kind,
      mimeType: item.kind === 'image' ? 'image/png' : 'text/plain',
      text: item.kind === 'image' ? null : item.preview,
      byteSize: item.byteSize,
      sourceAppName: item.sourceAppName,
      sourcePath:
        item.kind === 'file' ? '/synthetic/archiwum/notatka-syntetyczna.pdf' : null,
      sourceExists: false,
    };
  },
  setPinned: async (eventId, pinned) => {
    const index = historyItems.findIndex((item) => item.eventId === eventId);
    if (index < 0) throw new Error('history_event_not_found');
    historyItems = historyItems.map((item, itemIndex) =>
      itemIndex === index ? { ...item, pinned } : item,
    );
  },
  deleteEvent: async (eventId) => {
    const remaining = historyItems.filter((item) => item.eventId !== eventId);
    if (remaining.length === historyItems.length) throw new Error('history_event_not_found');
    historyItems = remaining;
  },
  copyEvent: async (_eventId, plainText) => ({ mode: 'copied', plainText }),
  // The browser preview offers the encrypted shape, so the password step is
  // reachable without a real export.
  chooseImportFile: async () => 'synthetic://clipboard-export.rayconfig',
  chooseImportDirectory: async () => 'synthetic://clipboard-export',
  // A path ending in .rayconfig demands a password, so the browser preview
  // walks the same steps the encrypted flow does on a real export.
  analyzeImport: async (path, password) => {
    if (path.toLowerCase().endsWith('.rayconfig')) {
      if (password === undefined) throw 'rayconfig_password_required';
      if (password !== 'synthetic') throw 'rayconfig_password_invalid';
    }
    return {
      analysisId: SYNTHETIC_IMPORT_PROGRESS.runId,
      total: 3,
      candidateRecords: 3,
      skipped: 0,
      failed: 0,
    };
  },
  startImport: async (_analysisId) => ({ runId: SYNTHETIC_IMPORT_PROGRESS.runId }),
  discardImportAnalysis: async (_analysisId) => undefined,
  getImportStatus: async (_runId) => ({
    ...SYNTHETIC_IMPORT_PROGRESS,
    summary: SYNTHETIC_IMPORT_PROGRESS.summary
      ? { ...SYNTHETIC_IMPORT_PROGRESS.summary }
      : null,
  }),
  revealSource: async () => {
    throw new Error('source_unavailable');
  },
  // Nothing to open in a browser preview; the settings page is reachable by
  // hand at #settings.
  openSettingsWindow: async () => undefined,
  linkPreview: async (eventId) => {
    const item = historyItems.find((candidate) => candidate.eventId === eventId);
    if (!item || item.kind !== 'link') return null;
    return {
      host: 'example.invalid',
      rest: '/synthetic-document',
      title: 'Synthetic document title',
      iconMime: null,
      iconBase64: null,
      imageMime: null,
      imageBase64: null,
      localOnly: false,
      fetching: false,
    };
  },
  chooseExportDirectory: async () => 'synthetic://clipboard-export',
  exportHistory: async () => ({ records: 4, images: 1, withoutPayload: 1 }),
  // The browser preview has no vault behind it; the settings pane still gets
  // a list to show, and copies that went nowhere but never fail.
  keyvaultList: async () =>
    SYNTHETIC_KEYVAULT_SECRETS.map((secret) => ({ ...secret })),
  keyvaultCopySecret: async () => undefined,
  // The browser preview has no core behind it, so nothing ever changes.
  onHistoryChanged: () => () => undefined,
  onLinkPreviewReady: () => () => undefined,
  getThumbnail: async (eventId) =>
    eventId === 103 ? { mimeType: 'image/png', base64: 'c3ludGhldGlj' } : null,
  getSettings: async () => ({
    ...settings,
    denylistedApps: [...settings.denylistedApps],
    keyvault: { ...settings.keyvault },
  }),
  isAutostartEnabled: async () => autostartEnabled,
  setAutostartEnabled: async (enabled) => {
    autostartEnabled = enabled;
  },
  saveSettings: async (nextSettings) => {
    settings = {
      ...nextSettings,
      denylistedApps: [...nextSettings.denylistedApps],
      keyvault: { ...nextSettings.keyvault },
    };
    return {
      ...settings,
      denylistedApps: [...settings.denylistedApps],
      keyvault: { ...settings.keyvault },
    };
  },
  getStorageStats: async () => ({ ...SYNTHETIC_STORAGE_STATS }),
  // Copies, as everywhere else in this gateway: a consumer mutating its
  // answer must not bend the next one.
  listApps: async () => SYNTHETIC_APPS.map((app) => ({ ...app })),
  // A browser preview cannot start applications; resolving rather than
  // rejecting keeps the palette's flow walkable where Tauri is absent.
  launchApp: async () => undefined,
  getAppIcon: async () => ({ ...SYNTHETIC_APP_ICON }),
};
