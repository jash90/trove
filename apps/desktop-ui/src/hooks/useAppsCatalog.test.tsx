import { act, renderHook, waitFor } from '@testing-library/react';
import { describe, expect, it, vi } from 'vitest';

import type { AppEntry } from '../lib/contracts';
import type { ClipboardGateway } from '../lib/gateway';
import { useAppsCatalog } from './useAppsCatalog';

const syntheticCatalog: AppEntry[] = [
  { name: 'Synthetic Notes', bundleId: 'com.example.notes', path: '/synthetic/Applications/Synthetic Notes.app' },
  { name: 'Synthetic Terminal', bundleId: null, path: '/synthetic/Applications/Synthetic Terminal.app' },
];

const makeGateway = (
  listApps: ClipboardGateway['listApps'],
): ClipboardGateway => ({ listApps }) as unknown as ClipboardGateway;

describe('useAppsCatalog', () => {
  // One gateway per test, not per render: the hook refetches when the gateway
  // identity changes, exactly like the real provider hands it a stable one.
  it('loads once while the mode stays enabled', async () => {
    const listApps = vi.fn(async () => syntheticCatalog);
    const gateway = makeGateway(listApps);
    const { rerender } = renderHook((enabled: boolean = true) =>
      useAppsCatalog(gateway, enabled),
    );

    await waitFor(() =>
      expect(listApps).toHaveBeenCalledOnce(),
    );
    rerender(true);
    rerender(true);

    expect(listApps).toHaveBeenCalledOnce();
  });

  it('surfaces errors without their details', async () => {
    const listApps = vi.fn(async () => {
      throw new Error('/private/var/folders/... contains a local path');
    });
    const gateway = makeGateway(listApps);
    const { result } = renderHook(() => useAppsCatalog(gateway, true));

    await waitFor(() => expect(result.current.status).toBe('error'));

    // A status, never the rejection text: local paths are not for the screen.
    expect(result.current.apps).toEqual([]);
    expect(result.current).not.toHaveProperty('message');
  });

  it('refetches when the mode is re-entered', async () => {
    const listApps = vi.fn(async () => syntheticCatalog);
    const gateway = makeGateway(listApps);
    const { rerender, result } = renderHook((enabled: boolean) =>
      useAppsCatalog(gateway, enabled),
    );

    rerender(true);
    await waitFor(() => expect(result.current.status).toBe('ready'));
    rerender(false);
    // The catalog stays on screen while the mode is hidden.
    expect(result.current.apps).toEqual(syntheticCatalog);
    rerender(true);
    await waitFor(() => expect(listApps).toHaveBeenCalledTimes(2));
  });

  it('ignores a late answer that arrives after the mode closed', async () => {
    let resolveList: (catalog: AppEntry[]) => void = () => undefined;
    const listApps = vi.fn(
      () =>
        new Promise<AppEntry[]>((resolve) => {
          resolveList = resolve;
        }),
    );
    const gateway = makeGateway(listApps);
    const { result, rerender } = renderHook((enabled: boolean) =>
      useAppsCatalog(gateway, enabled),
    );

    rerender(true);
    rerender(false);
    await act(async () => {
      resolveList(syntheticCatalog);
    });

    expect(result.current.apps).toEqual([]);
  });
});
