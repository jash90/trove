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
  missingPayload: boolean;
  hasThumbnail: boolean;
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
  missingPayload: boolean;
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

export interface AppSettings {
  schemaVersion: 1;
  hotkey: string;
  autostart: boolean;
  retentionDays: number | null;
  denylistedApps: string[];
}

export interface StorageStats {
  contentCount: number;
  eventCount: number;
  missingPayloadCount: number;
  databaseBytes: number;
  blobBytes: number;
}

export interface ImportRunHandle {
  runId: string;
}

export interface ImportAnalysis {
  analysisId: string;
  total: number;
  candidateRecords: number;
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

export function validateImportProgress(progress: ImportProgress): ImportProgress {
  const invalid = (): never => {
    throw new Error('invalid_import_progress');
  };
  if (progress.runId.trim().length === 0) invalid();
  const counters = [
    progress.processed,
    progress.total,
    progress.imported,
    progress.alreadyPresent,
    progress.skipped,
    progress.failed,
  ];
  if (!counters.every((counter) => Number.isSafeInteger(counter) && counter >= 0)) invalid();
  const outcomes =
    progress.imported + progress.alreadyPresent + progress.skipped + progress.failed;
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
  const summaryCounters = [
    summary.total,
    summary.imported,
    summary.alreadyPresent,
    summary.skipped,
    summary.failed,
  ];
  if (!summaryCounters.every((counter) => Number.isSafeInteger(counter) && counter >= 0)) invalid();
  if (
    summary.runId !== progress.runId ||
    summary.total !== progress.total ||
    summary.imported !== progress.imported ||
    summary.alreadyPresent !== progress.alreadyPresent ||
    summary.skipped !== progress.skipped ||
    summary.failed !== progress.failed ||
    summary.imported + summary.alreadyPresent + summary.skipped + summary.failed !== summary.total
  ) {
    invalid();
  }
  return progress;
}
