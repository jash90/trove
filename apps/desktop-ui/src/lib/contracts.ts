export type ContentKind = 'text' | 'link' | 'image' | 'file' | 'color' | 'code' | 'html';

export interface HistoryCursor {
  capturedAtMs: number;
  eventId: number;
}

export interface SearchRequest {
  query: string;
  limit: number;
  cursor: HistoryCursor | null;
  includeDoNotIndex?: boolean;
}

export interface HistoryItem {
  eventId: number;
  globalId: string;
  kind: ContentKind;
  capturedAtMs: number;
  sourceAppName: string | null;
  pinned: boolean;
  preview: string;
  byteSize: number;
  hasThumbnail: boolean;
  /** How many captures of this content are recorded, summed over its events. */
  occurrenceCount: number;
  /** When this content was captured, newest first, capped by the store. */
  occurrences: number[];
}

export interface HistoryPage {
  items: HistoryItem[];
  nextCursor: HistoryCursor | null;
  rankedTruncated: boolean;
}

export interface Preview {
  eventId: number;
  kind: ContentKind;
  mimeType: string;
  text: string | null;
  byteSize: number;
  sourceAppName: string | null;
  /** Where the entry came from, when its source recorded a location. */
  sourcePath: string | null;
  /** Whether that location still resolves on this machine. */
  sourceExists: boolean;
}

export type CopyMode =
  | 'copied'
  | 'pasted'
  | 'copied_only_permission_required'
  | 'copied_only_target_lost'
  | 'copied_only_platform_limit';

export interface CopyResult {
  mode: CopyMode;
  plainText: boolean;
}

export interface Thumbnail {
  mimeType: string;
  base64: string;
}

/**
 * Optional overrides for the vault connection.
 *
 * The connection itself — address, token and the private key — comes from the
 * device's shared identity file (`~/.config/keyvault/agent.json`), so no key is
 * stored by this application at all. These point one install somewhere else,
 * at a local backend for instance, and are normally both null.
 *
 * `privateJwk` is only ever read back from rows an older version wrote; the
 * next save drops it.
 */
export interface KeyvaultSettings {
  url: string | null;
  token: string | null;
  privateJwk?: string | null;
}

export interface AppSettings {
  schemaVersion: 1;
  hotkey: string;
  autostart: boolean;
  retentionDays: number | null;
  denylistedApps: string[];
  linkPreviews: boolean;
  keyvault: KeyvaultSettings;
}

/**
 * What stands between the user and the shortcut they configured.
 *
 * Two failures that feel identical from the keyboard and have different fixes.
 * `registered` false means the system refused the binding outright, usually
 * because another application holds it. `heldBySystem` means the binding took
 * and a system shortcut still eats every press — the ordinary state of ⌘Space
 * on a machine nobody has changed.
 */
export interface ShortcutStatus {
  hotkey: string;
  registered: boolean;
  heldBySystem: boolean;
  /** The system shortcuts this application turned off, so they can be given back. */
  releasedIds: number[];
}

/** How a request to change the system's shortcut table actually went. */
export type ShortcutRelease = 'alreadyFree' | 'applied' | 'needsLogout' | 'refused';

/**
 * Where a pairing stands.
 *
 * `idle` means nothing is in flight — the answer after a pairing finished or was abandoned, so
 * the interface can stop its timer without needing to remember why.
 */
/**
 * A pairing waiting for approval.
 *
 * The link and the code both travel because the browser that opened may not be the one someone is
 * signed into: a session lives in one browser's storage, and without something to copy a pairing
 * started in the wrong browser is a dead end.
 */
export interface PairingStarted {
  fingerprint: string;
  url: string;
  code: string;
  /**
   * When the code stops working, if the vault said. Optional because it is a nicety: a countdown
   * that can fail a pairing is worse than no countdown, which is what happened when this was
   * required and the value arrived as a float.
   */
  expiresAt: number | null;
}

/** Whether this device has paired, and which vault it knows. */
export interface KeyvaultIdentity {
  paired: boolean;
  url: string | null;
}

export type PairingStatus =
  | 'idle'
  | 'pending'
  | 'paired'
  | 'expired'
  | 'notFound'
  | 'alreadyClaimed';

/** One secret's metadata — everything the vault says without opening it. */
export interface KeyvaultSecret {
  slug: string;
  name: string;
  category?: string | null;
}

export interface StorageStats {
  contentCount: number;
  eventCount: number;
  databaseBytes: number;
  blobBytes: number;
}

export interface ImportRunHandle {
  runId: string;
}

export interface LinkPreview {
  host: string;
  rest: string;
  title: string | null;
  iconMime: string | null;
  iconBase64: string | null;
  /** The picture the page nominates for itself, downscaled. */
  imageMime: string | null;
  imageBase64: string | null;
  /** True when nothing was fetched and nothing will be. */
  localOnly: boolean;
  /**
   * True only while a fetch is under way and its result will be announced.
   *
   * Not derivable here: a page still being asked and a page that answered
   * without a picture both arrive with no image and localOnly false.
   */
  fetching: boolean;
}

export interface ExportSummary {
  records: number;
  images: number;
  /** Entries stored without a payload, written as metadata only. */
  withoutPayload: number;
}

export interface AppEntry {
  name: string;
  bundleId: string | null;
  /** The canonical filesystem path of the `.app` directory. */
  path: string;
}

/**
 * Mirrors of the bounds in crates/trove-launcher/src/lib.rs. Keep both
 * sides equal: a looser bound here can never fire and would hide a broken
 * contract instead of reporting it.
 */
export const MAX_APP_NAME_BYTES = 256;
export const MAX_APP_PATH_BYTES = 1_024;
export const MAX_CATALOG_APPS = 2_000;

export interface ImportAnalysis {
  analysisId: string;
  total: number;
  candidateRecords: number;
  /** Records left out because their source file is no longer there. */
  skipped: number;
  failed: number;
}

export interface ImportSummary {
  runId: string;
  total: number;
  imported: number;
  alreadyPresent: number;
  skipped: number;
  failed: number;
}

export type ImportRunState = 'running' | 'completed' | 'failed';

/**
 * Mirror of MAX_THUMBNAIL_BASE64_BYTES in src-tauri/src/commands.rs. Base64 is
 * ASCII, so the byte cap and the character cap are the same number. Keep both
 * sides equal: a looser bound here can never fire and would hide a broken
 * contract instead of reporting it.
 */
export const MAX_THUMBNAIL_BASE64_BYTES = 262_144;

/** How many entries one page of history holds. */
export const HISTORY_PAGE_SIZE = 80;

export interface ImportProgress {
  runId: string;
  state: ImportRunState;
  processed: number;
  total: number;
  imported: number;
  alreadyPresent: number;
  skipped: number;
  failed: number;
  errorCode: string | null;
  summary: ImportSummary | null;
}

const isSafeCount = (value: number): boolean =>
  Number.isSafeInteger(value) && value >= 0;

const safeCounterSum = (counters: readonly number[], errorCode: string): number => {
  const total = counters.reduce((sum, counter) => sum + counter, 0);
  if (!Number.isSafeInteger(total)) throw new Error(errorCode);
  return total;
};

export const validateImportAnalysis = (analysis: ImportAnalysis): ImportAnalysis => {
  if (
    typeof analysis?.analysisId !== 'string' ||
    analysis.analysisId.trim() !== analysis.analysisId ||
    analysis.analysisId.length === 0 ||
    ![analysis.total, analysis.candidateRecords, analysis.skipped, analysis.failed].every(
      isSafeCount,
    ) ||
    safeCounterSum(
      [analysis.candidateRecords, analysis.skipped, analysis.failed],
      'invalid_import_analysis',
    ) !== analysis.total
  ) {
    throw new Error('invalid_import_analysis');
  }
  return analysis;
};

export const validateImportSummary = (summary: ImportSummary): ImportSummary => {
  if (
    typeof summary?.runId !== 'string' ||
    summary.runId.trim() !== summary.runId ||
    summary.runId.length === 0
  ) {
    throw new Error('invalid_import_summary');
  }
  const counters = [
    summary.total,
    summary.imported,
    summary.alreadyPresent,
    summary.skipped,
    summary.failed,
  ];
  if (!counters.every(isSafeCount)) throw new Error('invalid_import_summary');
  const outcomes = safeCounterSum(counters.slice(1), 'invalid_import_summary');
  if (outcomes !== summary.total) throw new Error('invalid_import_summary');
  return summary;
};

export const validateStorageStats = (stats: StorageStats): StorageStats => {
  const counters = [
    stats?.contentCount,
    stats?.eventCount,
    stats?.databaseBytes,
    stats?.blobBytes,
  ];
  if (
    !counters.every(isSafeCount) ||
    !Number.isSafeInteger(stats.databaseBytes + stats.blobBytes)
  ) {
    throw new Error('invalid_storage_stats');
  }
  return stats;
};

const isAppEntry = (entry: unknown): entry is AppEntry =>
  typeof entry === 'object' &&
  entry !== null &&
  typeof (entry as AppEntry).name === 'string' &&
  (entry as AppEntry).name.length > 0 &&
  (entry as AppEntry).name.trim() === (entry as AppEntry).name &&
  (entry as AppEntry).name.length <= MAX_APP_NAME_BYTES &&
  typeof (entry as AppEntry).path === 'string' &&
  (entry as AppEntry).path.startsWith('/') &&
  (entry as AppEntry).path.length <= MAX_APP_PATH_BYTES &&
  ((entry as AppEntry).bundleId === null ||
    (typeof (entry as AppEntry).bundleId === 'string' &&
      (entry as AppEntry).bundleId!.length > 0));

/**
 * Checks the launcher catalog at the IPC boundary. The core already bounds
 * every field; this says so again on this side, because a contract only the
 * sender believes in is not a contract.
 */
export const validateAppCatalog = (entries: AppEntry[]): AppEntry[] => {
  if (!Array.isArray(entries) || entries.length > MAX_CATALOG_APPS) {
    throw new Error('invalid_app_catalog');
  }
  const seenPaths = new Set<string>();
  for (const entry of entries) {
    if (!isAppEntry(entry) || seenPaths.has(entry.path)) {
      throw new Error('invalid_app_catalog');
    }
    seenPaths.add(entry.path);
  }
  return entries;
};

export function validateImportProgress(
  progress: ImportProgress,
  expectedRunId: string = progress.runId,
): ImportProgress {
  const invalid = (): never => {
    throw new Error('invalid_import_progress');
  };
  if (
    typeof progress?.runId !== 'string' ||
    progress.runId.trim().length === 0 ||
    progress.runId !== expectedRunId
  ) {
    invalid();
  }
  const counters = [
    progress.processed,
    progress.total,
    progress.imported,
    progress.alreadyPresent,
    progress.skipped,
    progress.failed,
  ];
  if (!counters.every(isSafeCount)) invalid();
  const outcomes = safeCounterSum(
    [progress.imported, progress.alreadyPresent, progress.skipped, progress.failed],
    'invalid_import_progress',
  );
  if (progress.processed !== outcomes || progress.processed > progress.total) {
    invalid();
  }

  if (progress.state === 'running') {
    if (progress.summary !== null || progress.errorCode !== null) invalid();
    return progress;
  }

  if (progress.state === 'failed') {
    if (
      progress.summary !== null ||
      progress.errorCode === null ||
      progress.errorCode.trim().length === 0
    ) {
      invalid();
    }
    return progress;
  }

  if (progress.state !== 'completed') invalid();

  if (progress.errorCode !== null || outcomes !== progress.total) invalid();
  const summary = progress.summary;
  if (summary === null) throw new Error('invalid_import_progress');
  try {
    validateImportSummary(summary);
  } catch {
    invalid();
  }
  if (
    summary.runId !== progress.runId ||
    summary.total !== progress.total ||
    summary.imported !== progress.imported ||
    summary.alreadyPresent !== progress.alreadyPresent ||
    summary.skipped !== progress.skipped ||
    summary.failed !== progress.failed
  ) {
    invalid();
  }
  return progress;
}
