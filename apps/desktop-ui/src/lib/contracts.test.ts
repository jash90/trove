import { describe, expect, it } from 'vitest';

import {
  validateImportAnalysis,
  validateImportProgress,
  validateImportSummary,
  validateStorageStats,
  type ImportProgress,
} from './contracts';

const completed: ImportProgress = {
  runId: '0198f000-0000-7000-8000-000000000000',
  state: 'completed',
  processed: 10,
  total: 10,
  imported: 7,
  alreadyPresent: 1,
  skipped: 1,
  failed: 1,
  errorCode: null,
  summary: {
    runId: '0198f000-0000-7000-8000-000000000000',
    total: 10,
    imported: 7,
    alreadyPresent: 1,
    skipped: 1,
    failed: 1,
  },
};

describe('validateImportProgress', () => {
  it('accepts a completed run only when every source record is accounted for', () => {
    expect(validateImportProgress(completed, completed.runId)).toBe(completed);
  });

  it('rejects inconsistent processed and outcome counts', () => {
    expect(() => validateImportProgress({ ...completed, processed: 9 })).toThrow(
      'invalid_import_progress',
    );
  });

  it('rejects a completed run with an unaccounted record', () => {
    expect(() =>
      validateImportProgress({
        ...completed,
        processed: 9,
        imported: 6,
      }),
    ).toThrow('invalid_import_progress');
  });

  it.each([
    ['negative', { ...completed, failed: -1, processed: 8 }],
    ['infinite', { ...completed, total: Number.POSITIVE_INFINITY }],
    ['unsafe', { ...completed, total: Number.MAX_SAFE_INTEGER + 1 }],
  ])('rejects %s counters', (_label, progress) => {
    expect(() => validateImportProgress(progress)).toThrow('invalid_import_progress');
  });

  it('requires a nonempty run id and a completed summary', () => {
    expect(() => validateImportProgress({ ...completed, runId: '  ' })).toThrow(
      'invalid_import_progress',
    );
    expect(() => validateImportProgress({ ...completed, summary: null })).toThrow(
      'invalid_import_progress',
    );
    expect(() =>
      validateImportProgress({ ...completed, state: 'unknown' } as unknown as ImportProgress),
    ).toThrow('invalid_import_progress');
  });

  it('rejects summaries for running and failed states', () => {
    expect(() =>
      validateImportProgress({
        ...completed,
        state: 'running',
        processed: 9,
        imported: 6,
      }),
    ).toThrow('invalid_import_progress');
    expect(() =>
      validateImportProgress({
        ...completed,
        state: 'failed',
        errorCode: 'import_failed',
      }),
    ).toThrow('invalid_import_progress');
  });

  it('requires exact run and counter parity in the completed summary', () => {
    expect(() =>
      validateImportProgress({
        ...completed,
        summary: { ...completed.summary!, runId: 'different-run' },
      }),
    ).toThrow('invalid_import_progress');
    expect(() =>
      validateImportProgress({
        ...completed,
        summary: { ...completed.summary!, imported: 6, skipped: 2 },
      }),
    ).toThrow('invalid_import_progress');
  });

  it('rejects a status response for a different requested run', () => {
    expect(() => validateImportProgress(completed, 'different-run')).toThrow(
      'invalid_import_progress',
    );
  });

  it('accepts only native-shaped running and failed progress', () => {
    const running: ImportProgress = {
      ...completed,
      state: 'running',
      processed: 9,
      imported: 6,
      summary: null,
    };
    const failed: ImportProgress = {
      ...running,
      state: 'failed',
      errorCode: 'import_failed',
    };

    expect(validateImportProgress(running)).toBe(running);
    expect(validateImportProgress(failed)).toBe(failed);
    expect(() => validateImportProgress({ ...failed, errorCode: '' })).toThrow(
      'invalid_import_progress',
    );
  });
});

describe('validateImportAnalysis', () => {
  it('accepts only safe count-only analyses with exact accounting', () => {
    const analysis = {
      analysisId: '0198f000-0000-7000-8000-000000000000',
      total: 6_503,
      candidateRecords: 6_500,
      skipped: 0,
      failed: 3,
    };

    expect(validateImportAnalysis(analysis)).toBe(analysis);
  });

  it.each([
    ['blank id', { analysisId: ' ', total: 1, candidateRecords: 1, skipped: 0, failed: 0 }],
    ['fraction', { analysisId: 'a', total: 1.5, candidateRecords: 1, skipped: 0, failed: 0 }],
    [
      'unsafe count',
      {
        analysisId: 'a',
        total: Number.MAX_SAFE_INTEGER + 1,
        candidateRecords: 1,
        skipped: 0,
        failed: 0,
      },
    ],
    ['missing accounting', { analysisId: 'a', total: 3, candidateRecords: 1, skipped: 0, failed: 1 }],
  ])('rejects a malformed analysis: %s', (_label, analysis) => {
    expect(() => validateImportAnalysis(analysis)).toThrow('invalid_import_analysis');
  });
});

describe('validateImportSummary', () => {
  it('keeps already-present records as a separate terminal outcome', () => {
    const summary = {
      runId: '0198f000-0000-7000-8000-000000000000',
      total: 10,
      imported: 6,
      alreadyPresent: 2,
      skipped: 1,
      failed: 1,
    };

    expect(validateImportSummary(summary)).toBe(summary);
  });

  it('requires every terminal outcome to sum exactly to total', () => {
    expect(() =>
      validateImportSummary({
        runId: '0198f000-0000-7000-8000-000000000000',
        total: 10,
        imported: 8,
        alreadyPresent: 0,
        skipped: 1,
        failed: 0,
      }),
    ).toThrow('invalid_import_summary');
  });

  it('rejects unsafe summary counters', () => {
    expect(() =>
      validateImportSummary({
        runId: 'run',
        total: Number.MAX_SAFE_INTEGER + 1,
        imported: 0,
        alreadyPresent: 0,
        skipped: 0,
        failed: 0,
      }),
    ).toThrow('invalid_import_summary');
  });
});

describe('validateStorageStats', () => {
  it('accepts safe database and referenced-blob counts', () => {
    const stats = {
      contentCount: 4,
      eventCount: 5,
      databaseBytes: 1_024,
      blobBytes: 2_048,
    };

    expect(validateStorageStats(stats)).toBe(stats);
  });

  it.each([
    ['unsafe count', { contentCount: Number.MAX_SAFE_INTEGER + 1 }],
    ['unsafe byte sum', { databaseBytes: Number.MAX_SAFE_INTEGER, blobBytes: 1 }],
  ])('rejects unsafe storage stats: %s', (_label, overrides) => {
    expect(() =>
      validateStorageStats({
        contentCount: 4,
        eventCount: 5,
        databaseBytes: 1_024,
        blobBytes: 2_048,
        ...overrides,
      }),
    ).toThrow('invalid_storage_stats');
  });
});
