import { describe, expect, it } from 'vitest';

import { validateImportProgress, type ImportProgress } from './contracts';

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
    expect(validateImportProgress(completed)).toBe(completed);
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
