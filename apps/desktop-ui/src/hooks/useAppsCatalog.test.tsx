import { act, renderHook, waitFor } from '@testing-library/react';
import { beforeEach, describe, expect, it, vi } from 'vitest';

import type { AppEntry } from '../lib/contracts';
import type { ClipboardGateway } from '../lib/gateway';
import { useAppsCatalog } from './useAppsCatalog';

/// The Tauri window, replaced: the palette is hidden and shown, never
/// remounted, and the hook's refetch-on-focus is what a new opening is.
/// Subscribers are collected so a test can fire the event, and the unlisten
/// removes them, the way a real focus subscription comes and goes.
type FocusListener = (event: { payload: boolean }) => void;
const focusListeners: FocusListener[] = [];

vi.mock('@tauri-apps/api/window', () => ({
  getCurrentWindow: () => ({
    onFocusChanged: (listener: FocusListener) => {
      focusListeners.push(listener);
      return Promise.resolve(() => {
        const index = focusListeners.indexOf(listener);
        if (index >= 0) focusListeners.splice(index, 1);
      });
    },
  }),
}));

const gainFocus = async (): Promise<void> => {
  await act(async () => {
    for (const listener of [...focusListeners]) listener({ payload: true });
  });
};

const syntheticCatalog: AppEntry[] = [
  { name: 'Synthetic Notes', bundleId: 'com.example.notes', path: '/synthetic/Applications/Synthetic Notes.app' },
  { name: 'Synthetic Terminal', bundleId: null, path: '/synthetic/Applications/Synthetic Terminal.app' },
];

const makeGateway = (
  listApps: ClipboardGateway['listApps'],
  onAppsChanged?: ClipboardGateway['onAppsChanged'],
): ClipboardGateway => ({ listApps, onAppsChanged }) as unknown as ClipboardGateway;

beforeEach(() => {
  focusListeners.length = 0;
});

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

  it('keeps the previous catalog on screen while a refetch is in flight', async () => {
    let resolveSecond: (catalog: AppEntry[]) => void = () => undefined;
    const listApps = vi.fn()
      .mockImplementationOnce(async () => syntheticCatalog)
      .mockImplementationOnce(
        () =>
          new Promise<AppEntry[]>((resolve) => {
            resolveSecond = resolve;
          }),
      );
    const gateway = makeGateway(listApps);
    const { result } = renderHook(() => useAppsCatalog(gateway, true));

    await waitFor(() => expect(result.current.status).toBe('ready'));
    expect(result.current.apps).toEqual(syntheticCatalog);

    await gainFocus();

    // The summoning refetch started, but the rows the user is looking at
    // did not leave the screen while its answer was on its way.
    expect(listApps).toHaveBeenCalledTimes(2);
    expect(result.current.status).toBe('ready');
    expect(result.current.apps).toEqual(syntheticCatalog);

    await act(async () => {
      resolveSecond([{ name: 'New', bundleId: null, path: '/synthetic/Applications/New.app' }]);
    });
    expect(result.current.apps).toEqual([
      { name: 'New', bundleId: null, path: '/synthetic/Applications/New.app' },
    ]);
  });

  it('keeps the previous catalog when a refresh fails', async () => {
    const listApps = vi.fn()
      .mockImplementationOnce(async () => syntheticCatalog)
      .mockImplementationOnce(async () => {
        throw new Error('apps_unavailable');
      });
    const gateway = makeGateway(listApps);
    const { result } = renderHook(() => useAppsCatalog(gateway, true));

    await waitFor(() => expect(result.current.status).toBe('ready'));
    await gainFocus();
    await waitFor(() => expect(listApps).toHaveBeenCalledTimes(2));

    // A failed refresh is no reason to take the launcher away: the stale
    // catalog stays answerable and the next summoning tries again.
    expect(result.current.status).toBe('ready');
    expect(result.current.apps).toEqual(syntheticCatalog);
  });

  it('refetches when the palette window gains focus', async () => {
    const listApps = vi.fn(async () => syntheticCatalog);
    const gateway = makeGateway(listApps);
    renderHook(() => useAppsCatalog(gateway, true));

    await waitFor(() => expect(listApps).toHaveBeenCalledOnce());
    // Let the focus subscription land before firing it.
    await act(async () => {});

    await gainFocus();
    expect(listApps).toHaveBeenCalledTimes(2);

    // Losing focus — the palette hidden — asks for nothing.
    await act(async () => {
      for (const listener of [...focusListeners]) listener({ payload: false });
    });
    expect(listApps).toHaveBeenCalledTimes(2);
  });

  it('refetches when the core announces the catalog changed', async () => {
    let announce: (() => void) | null = null;
    const listApps = vi.fn(async () => syntheticCatalog);
    const gateway = makeGateway(listApps, (listener) => {
      announce = listener;
      return () => {
        announce = null;
      };
    });
    const { result } = renderHook(() => useAppsCatalog(gateway, true));

    await waitFor(() => expect(result.current.status).toBe('ready'));
    expect(announce).not.toBeNull();

    const fresh: AppEntry[] = [
      { name: 'Freshly Installed', bundleId: null, path: '/synthetic/Applications/Freshly Installed.app' },
    ];
    listApps.mockImplementation(async () => fresh);
    await act(async () => {
      announce?.();
    });

    await waitFor(() => expect(result.current.apps).toEqual(fresh));
    expect(listApps).toHaveBeenCalledTimes(2);
  });
});
