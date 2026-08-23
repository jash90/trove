import { describe, expect, it, vi } from 'vitest';

import { SYNTHETIC_HISTORY_ITEMS } from './fixtures';
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
});
