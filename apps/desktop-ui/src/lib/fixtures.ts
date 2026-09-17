import type {
  AppEntry,
  AppSettings,
  HistoryItem,
  ImportProgress,
  StorageStats,
  Thumbnail,
} from './contracts';

export const SYNTHETIC_HISTORY_ITEMS: HistoryItem[] = [
  {
    eventId: 101,
    globalId: '0198f000-0000-7000-8000-000000000101',
    kind: 'text',
    capturedAtMs: 1_775_000_000_000,
    sourceAppName: 'Synthetic Editor',
    pinned: true,
    preview: 'Synthetic project note for browser preview',
    byteSize: 42,
    hasThumbnail: false,
    // Pasted over three days: one row in the list, three timestamps in the
    // preview.
    occurrenceCount: 3,
    occurrences: [1_775_000_000_000, 1_774_900_000_000, 1_774_800_000_000],
  },
  {
    eventId: 102,
    globalId: '0198f000-0000-7000-8000-000000000102',
    kind: 'link',
    capturedAtMs: 1_774_999_940_000,
    sourceAppName: 'Synthetic Browser',
    pinned: false,
    preview: 'https://example.invalid/synthetic-document',
    byteSize: 42,
    hasThumbnail: false,
    occurrenceCount: 1,
    occurrences: [1_774_999_940_000],
  },
  {
    eventId: 103,
    globalId: '0198f000-0000-7000-8000-000000000103',
    kind: 'image',
    capturedAtMs: 1_774_999_880_000,
    sourceAppName: 'Synthetic Canvas',
    pinned: false,
    preview: 'Synthetic image · 640 × 480',
    byteSize: 24_576,
    hasThumbnail: true,
    occurrenceCount: 1,
    occurrences: [1_774_999_880_000],
  },
  {
    eventId: 104,
    globalId: '0198f000-0000-7000-8000-000000000104',
    kind: 'file',
    capturedAtMs: 1_774_999_820_000,
    sourceAppName: 'Synthetic Finder',
    pinned: false,
    preview: 'raport-syntetyczny.pdf',
    byteSize: 0,
    hasThumbnail: false,
    occurrenceCount: 1,
    occurrences: [1_774_999_820_000],
  },
];

export const SYNTHETIC_SETTINGS: AppSettings = {
  schemaVersion: 1,
  hotkey: 'CommandOrControl+Space',
  autostart: false,
  // History is unbounded unless the user deliberately enables retention.
  retentionDays: null,
  denylistedApps: ['com.apple.Passwords', 'com.apple.keychainaccess'],
  linkPreviews: true,
  paletteModes: true,
  keyvault: { url: null, token: null, privateJwk: null },
};

/** Metadata the vault would list for a configured token — never values. */
export const SYNTHETIC_KEYVAULT_SECRETS = [
  { slug: 'openai', name: 'OpenAI', category: 'ai' },
  { slug: 'github', name: 'GitHub', category: null },
];

export const SYNTHETIC_STORAGE_STATS: StorageStats = {
  contentCount: 4,
  eventCount: 4,
  databaseBytes: 49_152,
  blobBytes: 24_576,
};

/**
 * The launcher's browser-preview catalog: alphabetical, diacritics included,
 * small enough to read at a glance and varied enough to filter against —
 * a prefix match, a word-start match, a bundle-id-only match.
 */
export const SYNTHETIC_APPS: AppEntry[] = [
  { name: 'Finder', bundleId: 'com.apple.finder', path: '/synthetic/System/Library/CoreServices/Finder.app' },
  { name: 'Synthetic Browser', bundleId: 'com.example.browser', path: '/synthetic/Applications/Synthetic Browser.app' },
  { name: 'Synthetic Notes', bundleId: 'com.example.notes', path: '/synthetic/Applications/Synthetic Notes.app' },
  { name: 'Synthetic Terminal', bundleId: null, path: '/synthetic/Applications/Utilities/Synthetic Terminal.app' },
  { name: 'Łódź Editor', bundleId: 'pl.example.lodz-editor', path: '/synthetic/Applications/Łódź Editor.app' },
];

/** A one-pixel transparent PNG: the stand-in icon every application shares. */
export const SYNTHETIC_APP_ICON: Thumbnail = {
  mimeType: 'image/png',
  base64:
    'iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKsMIgAAAABJRU5ErkJggg==',
};

export const SYNTHETIC_IMPORT_PROGRESS: ImportProgress = {
  runId: '0198f000-0000-7000-8000-000000000201',
  state: 'completed',
  processed: 3,
  total: 3,
  imported: 3,
  alreadyPresent: 0,
  skipped: 0,
  failed: 0,
  errorCode: null,
  summary: {
    runId: '0198f000-0000-7000-8000-000000000201',
    total: 3,
    imported: 3,
    alreadyPresent: 0,
    skipped: 0,
    failed: 0,
  },
};
