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
});
