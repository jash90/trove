import { describe, expect, it, vi } from 'vitest';

import { SYNTHETIC_APPS, SYNTHETIC_HISTORY_ITEMS } from './fixtures';
import { mockGateway } from './mockGateway';

describe('mockGateway', () => {
  it('loads without reading native browser globals at module evaluation', async () => {
    vi.resetModules();
    vi.stubGlobal('window', undefined);

    await expect(import('./mockGateway')).resolves.toHaveProperty('mockGateway');

    vi.unstubAllGlobals();
  });

  it('returns only synthetic fixtures through the same bounded search contract', async () => {
    const page = await mockGateway.search({ query: '', limit: 80, cursor: null });

    expect(page.items).toEqual(SYNTHETIC_HISTORY_ITEMS);
    expect(page.items.every((item) => item.sourceAppName?.startsWith('Synthetic') ?? true)).toBe(
      true,
    );
  });

  it('returns the deterministic synthetic application catalog as copies', async () => {
    const first = await mockGateway.listApps();
    const second = await mockGateway.listApps();

    expect(first).toEqual(SYNTHETIC_APPS);
    // Copies, not the fixture objects: a consumer mutating its answer must not
    // bend the next one.
    expect(first[0]).not.toBe(SYNTHETIC_APPS[0]);
    expect(first[0]).not.toBe(second[0]);
  });

  it('launches nothing in the browser preview but keeps the promise', async () => {
    await expect(
      mockGateway.launchApp('/synthetic/Applications/Synthetic Notes.app'),
    ).resolves.toBeUndefined();
  });
});
