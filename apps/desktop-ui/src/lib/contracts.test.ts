import { describe, expect, it } from 'vitest';

import {
  MAX_APP_NAME_BYTES,
  MAX_APP_PATH_BYTES,
  MAX_CATALOG_APPS,
  validateAppCatalog,
  validateImportAnalysis,
  validateImportProgress,
  validateImportSummary,
  validateStorageStats,
  type AppEntry,
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

describe('validateAppCatalog', () => {
  const catalog: AppEntry[] = [
    { name: 'Synthetic Notes', bundleId: 'com.example.notes', path: '/synthetic/Applications/Synthetic Notes.app' },
    { name: 'Stem Only', bundleId: null, path: '/synthetic/Applications/Stem Only.app' },
  ];

  it('accepts a catalog that honors the Rust bounds', () => {
    expect(validateAppCatalog(catalog)).toBe(catalog);
  });

  it.each([
    ['an empty name', [{ name: '', bundleId: null, path: '/synthetic/Applications/A.app' }]],
    ['a padded name', [{ name: ' Padded', bundleId: null, path: '/synthetic/Applications/A.app' }]],
    [
      'an oversized name',
      [{ name: 'x'.repeat(300), bundleId: null, path: '/synthetic/Applications/A.app' }],
    ],
    ['a relative path', [{ name: 'A', bundleId: null, path: 'Applications/A.app' }]],
    [
      'an oversized path',
      [{ name: 'A', bundleId: null, path: `/${'a'.repeat(2_000)}/A.app` }],
    ],
    [
      'duplicate paths',
      [
        { name: 'A', bundleId: null, path: '/synthetic/Applications/A.app' },
        { name: 'A Again', bundleId: null, path: '/synthetic/Applications/A.app' },
      ],
    ],
    ['an empty bundle id', [{ name: 'A', bundleId: '', path: '/synthetic/Applications/A.app' }]],
    ['a non-string name', [{ name: 7, bundleId: null, path: '/synthetic/Applications/A.app' }]],
  ])('rejects %s', (_label, entries) => {
    expect(() => validateAppCatalog(entries as AppEntry[])).toThrow('invalid_app_catalog');
  });

  it('rejects a catalog past the Rust cap', () => {
    const flooded = Array.from(
      { length: 2_001 },
      (_, index): AppEntry => ({
        name: `App ${index}`,
        bundleId: null,
        path: `/synthetic/Applications/App ${index}.app`,
      }),
    );

    expect(() => validateAppCatalog(flooded)).toThrow('invalid_app_catalog');
  });

  it('accepts entries and catalogs exactly at the Rust bounds', () => {
    // Exact boundary, not a value comfortably below it: the probes elsewhere
    // in this suite sit above the bounds, so they would stay green while
    // the two sides of the bridge drifted apart. This is the pin.
    const exact: AppEntry[] = [
      {
        name: 'a'.repeat(MAX_APP_NAME_BYTES),
        bundleId: null,
        // '/' + fill + '/' + 'A.app' must add up to exactly the path bound.
        path: `/${'b'.repeat(MAX_APP_PATH_BYTES - 7)}/A.app`,
      },
    ];
    expect(validateAppCatalog(exact)).toBe(exact);

    const full = Array.from(
      { length: MAX_CATALOG_APPS },
      (_, index): AppEntry => ({
        name: 'A',
        bundleId: null,
        path: `/synthetic/Applications/App ${index}.app`,
      }),
    );
    expect(validateAppCatalog(full)).toBe(full);
  });

  it('rejects one byte past each Rust bound', () => {
    const longName: AppEntry[] = [
      { name: 'a'.repeat(MAX_APP_NAME_BYTES + 1), bundleId: null, path: '/synthetic/Applications/A.app' },
    ];
    const longPath: AppEntry[] = [
      { name: 'A', bundleId: null, path: `/${'b'.repeat(MAX_APP_PATH_BYTES - 6)}/A.app` },
    ];

    expect(() => validateAppCatalog(longName)).toThrow('invalid_app_catalog');
    expect(() => validateAppCatalog(longPath)).toThrow('invalid_app_catalog');
  });

  it('pins the mirrored bound values themselves', () => {
    // The same assertion lives in crates/clipboard-launcher. A change must
    // update both tests, not silently drift one side of the bridge.
    expect(MAX_APP_NAME_BYTES).toBe(256);
    expect(MAX_APP_PATH_BYTES).toBe(1_024);
    expect(MAX_CATALOG_APPS).toBe(2_000);
  });
});
