import type { ClipboardGateway } from './gateway';
import {
  SYNTHETIC_HISTORY_ITEMS,
  SYNTHETIC_IMPORT_PROGRESS,
  SYNTHETIC_SETTINGS,
  SYNTHETIC_STORAGE_STATS,
} from './fixtures';

let historyItems = SYNTHETIC_HISTORY_ITEMS.map((item) => ({ ...item }));
let settings = { ...SYNTHETIC_SETTINGS, denylistedApps: [...SYNTHETIC_SETTINGS.denylistedApps] };

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
      missingPayload: item.missingPayload,
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
  analyzeImport: async (_path) => ({
    analysisId: '0198f000-0000-7000-8000-000000000200',
    total: 3,
    candidateRecords: 3,
    failed: 0,
  }),
  startImport: async (_analysisId) => ({ runId: SYNTHETIC_IMPORT_PROGRESS.runId }),
  discardImportAnalysis: async (_analysisId) => undefined,
  getImportStatus: async (_runId) => ({
    ...SYNTHETIC_IMPORT_PROGRESS,
    summary: SYNTHETIC_IMPORT_PROGRESS.summary
      ? { ...SYNTHETIC_IMPORT_PROGRESS.summary }
      : null,
  }),
  getThumbnail: async (eventId) =>
    eventId === 103 ? { mimeType: 'image/png', base64: 'c3ludGhldGlj' } : null,
  getSettings: async () => ({ ...settings, denylistedApps: [...settings.denylistedApps] }),
  saveSettings: async (nextSettings) => {
    settings = { ...nextSettings, denylistedApps: [...nextSettings.denylistedApps] };
    return { ...settings, denylistedApps: [...settings.denylistedApps] };
  },
  getStorageStats: async () => ({ ...SYNTHETIC_STORAGE_STATS }),
};
