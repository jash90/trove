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
  if (!progress.runId) throw new Error('invalid_import_progress');
  const outcomes =
    progress.imported + progress.alreadyPresent + progress.skipped + progress.failed;
  if (progress.processed !== outcomes || progress.processed > progress.total) {
    throw new Error('invalid_import_progress');
  }
  if (progress.state === 'completed' && outcomes !== progress.total) {
    throw new Error('invalid_import_progress');
  }
  return progress;
}
