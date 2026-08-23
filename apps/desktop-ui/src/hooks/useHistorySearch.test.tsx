import { act, renderHook } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import type { HistoryPage } from '../lib/contracts';
import type { ClipboardGateway } from '../lib/gateway';
import { useHistorySearch } from './useHistorySearch';

const makePage = (preview: string): HistoryPage => ({
  items: [
    {
      eventId: 1,
      globalId: '0198f000-0000-7000-8000-000000000001',
      kind: 'text',
      capturedAtMs: 1_775_000_000_000,
      sourceAppName: 'Synthetic Editor',
      pinned: false,
      preview,
      byteSize: preview.length,
      missingPayload: false,
      hasThumbnail: false,
    },
  ],
  nextCursor: null,
  rankedTruncated: false,
});

interface Deferred<T> {
  promise: Promise<T>;
  resolve: (value: T) => void;
  reject: (reason?: unknown) => void;
}

const deferred = <T,>(): Deferred<T> => {
  let resolve!: (value: T) => void;
  let reject!: (reason?: unknown) => void;
  const promise = new Promise<T>((promiseResolve, promiseReject) => {
    resolve = promiseResolve;
    reject = promiseReject;
  });
  return { promise, resolve, reject };
};

const gatewayWithSearch = (
  search: ClipboardGateway['search'],
): ClipboardGateway =>
  ({ search }) as ClipboardGateway;

describe('useHistorySearch', () => {
  beforeEach(() => {
    vi.useFakeTimers();
  });

  afterEach(() => {
    window.onunhandledrejection = null;
    vi.restoreAllMocks();
    vi.useRealTimers();
  });

  it('keeps the newest search result when an older request resolves later', async () => {
    const oldRequest = deferred<HistoryPage>();
    const newestRequest = deferred<HistoryPage>();
    const search = vi.fn<ClipboardGateway['search']>((request) =>
      request.query === 'old' ? oldRequest.promise : newestRequest.promise,
    );
    const gateway = gatewayWithSearch(search);
    const { result } = renderHook(() => useHistorySearch(gateway));

    act(() => {
      result.current.setQuery('old');
    });
    await act(async () => {
      await vi.advanceTimersByTimeAsync(150);
    });
    act(() => {
      result.current.setQuery('newest');
    });
    await act(async () => {
      await vi.advanceTimersByTimeAsync(150);
      newestRequest.resolve(makePage('newest'));
      await Promise.resolve();
    });

    expect(result.current.items[0]?.preview).toBe('newest');

    await act(async () => {
      oldRequest.resolve(makePage('old'));
      await Promise.resolve();
    });

    expect(result.current.items[0]?.preview).toBe('newest');
  });

  it('keeps ready state when an older request rejects after the newest succeeds', async () => {
    const oldRequest = deferred<HistoryPage>();
    const newestRequest = deferred<HistoryPage>();
    const gateway = gatewayWithSearch((request) =>
      request.query === 'old' ? oldRequest.promise : newestRequest.promise,
    );
    const { result } = renderHook(() => useHistorySearch(gateway));

    act(() => {
      result.current.setQuery('old');
    });
    await act(async () => {
      await vi.advanceTimersByTimeAsync(150);
    });
    act(() => {
      result.current.setQuery('newest');
    });
    await act(async () => {
      await vi.advanceTimersByTimeAsync(150);
      newestRequest.resolve(makePage('newest'));
      await Promise.resolve();
    });

    await act(async () => {
      oldRequest.reject(new Error('private query and path must stay internal'));
      await Promise.resolve();
    });

    expect(result.current.status).toBe('ready');
    expect(result.current.items[0]?.preview).toBe('newest');
    expect(result.current).not.toHaveProperty('error');
  });

  it('debounces an empty default-history request for exactly 150 ms and bounds it', async () => {
    const request = deferred<HistoryPage>();
    const search = vi.fn<ClipboardGateway['search']>(() => request.promise);
    const gateway = gatewayWithSearch(search);
    const { result } = renderHook(() => useHistorySearch(gateway));

    await act(async () => {
      await vi.advanceTimersByTimeAsync(149);
    });
    expect(search).not.toHaveBeenCalled();
    expect(result.current.status).toBe('loading');

    await act(async () => {
      await vi.advanceTimersByTimeAsync(1);
    });
    expect(search).toHaveBeenCalledWith({ query: '', limit: 80, cursor: null });

    await act(async () => {
      request.resolve({ items: [], nextCursor: null, rankedTruncated: false });
      await Promise.resolve();
    });
    expect(result.current.status).toBe('ready');
    expect(result.current.items).toEqual([]);
  });

  it('invalidates an in-flight query when the query is cleared', async () => {
    const privateRequest = deferred<HistoryPage>();
    const defaultRequest = deferred<HistoryPage>();
    const gateway = gatewayWithSearch((request) =>
      request.query === '' ? defaultRequest.promise : privateRequest.promise,
    );
    const { result } = renderHook(() => useHistorySearch(gateway));

    act(() => {
      result.current.setQuery('sensitive search');
    });
    await act(async () => {
      await vi.advanceTimersByTimeAsync(150);
    });
    act(() => {
      result.current.setQuery('');
    });
    await act(async () => {
      await vi.advanceTimersByTimeAsync(150);
      defaultRequest.resolve(makePage('default history'));
      await Promise.resolve();
    });
    await act(async () => {
      privateRequest.resolve(makePage('stale private result'));
      await Promise.resolve();
    });

    expect(result.current.query).toBe('');
    expect(result.current.items[0]?.preview).toBe('default history');
  });

  it('invalidates an in-flight request when the gateway changes', async () => {
    const firstRequest = deferred<HistoryPage>();
    const secondRequest = deferred<HistoryPage>();
    const firstGateway = gatewayWithSearch(() => firstRequest.promise);
    const secondGateway = gatewayWithSearch(() => secondRequest.promise);
    const { result, rerender } = renderHook(
      ({ gateway }) => useHistorySearch(gateway),
      { initialProps: { gateway: firstGateway } },
    );

    await act(async () => {
      await vi.advanceTimersByTimeAsync(150);
    });
    rerender({ gateway: secondGateway });
    await act(async () => {
      await vi.advanceTimersByTimeAsync(150);
      secondRequest.resolve(makePage('second gateway'));
      await Promise.resolve();
    });
    await act(async () => {
      firstRequest.resolve(makePage('first gateway'));
      await Promise.resolve();
    });

    expect(result.current.items[0]?.preview).toBe('second gateway');
  });

  it('clears a pending debounce when unmounted', async () => {
    const search = vi.fn<ClipboardGateway['search']>();
    const gateway = gatewayWithSearch(search);
    const { unmount } = renderHook(() => useHistorySearch(gateway));

    unmount();
    await vi.advanceTimersByTimeAsync(150);

    expect(search).not.toHaveBeenCalled();
  });

  it('ignores an in-flight success after unmount without reporting a state leak', async () => {
    const request = deferred<HistoryPage>();
    const search = vi.fn<ClipboardGateway['search']>(() => request.promise);
    const gateway = gatewayWithSearch(search);
    const consoleError = vi.spyOn(console, 'error').mockImplementation(() => undefined);
    const { result, unmount } = renderHook(() => useHistorySearch(gateway));

    await act(async () => {
      await vi.advanceTimersByTimeAsync(150);
    });
    const stateAtUnmount = result.current;
    unmount();

    await act(async () => {
      request.resolve(makePage('ignored after unmount'));
      await Promise.resolve();
    });

    expect(search).toHaveBeenCalledOnce();
    expect(result.current).toBe(stateAtUnmount);
    expect(result.current.status).toBe('loading');
    expect(result.current.items).toEqual([]);
    expect(result.current).not.toHaveProperty('error');
    expect(consoleError).not.toHaveBeenCalled();
  });

  it('safely ignores an in-flight rejection after unmount', async () => {
    const request = deferred<HistoryPage>();
    const gateway = gatewayWithSearch(() => request.promise);
    const unhandledRejection = vi.fn();
    const consoleError = vi.spyOn(console, 'error').mockImplementation(() => undefined);
    window.onunhandledrejection = unhandledRejection;
    const { result, unmount } = renderHook(() => useHistorySearch(gateway));

    await act(async () => {
      await vi.advanceTimersByTimeAsync(150);
    });
    const stateAtUnmount = result.current;
    unmount();

    await act(async () => {
      request.reject(new Error('synthetic_sensitive_failure'));
      await Promise.resolve();
      await Promise.resolve();
    });

    expect(result.current).toBe(stateAtUnmount);
    expect(result.current.status).toBe('loading');
    expect(result.current.items).toEqual([]);
    expect(result.current).not.toHaveProperty('error');
    expect(unhandledRejection).not.toHaveBeenCalled();
    expect(consoleError).not.toHaveBeenCalled();
  });

  it('ignores an older rejection after the query is cleared', async () => {
    const queryRequest = deferred<HistoryPage>();
    const defaultRequest = deferred<HistoryPage>();
    const gateway = gatewayWithSearch((request) =>
      request.query === '' ? defaultRequest.promise : queryRequest.promise,
    );
    const { result } = renderHook(() => useHistorySearch(gateway));

    act(() => {
      result.current.setQuery('synthetic sensitive query');
    });
    await act(async () => {
      await vi.advanceTimersByTimeAsync(150);
    });
    act(() => {
      result.current.setQuery('');
    });
    await act(async () => {
      await vi.advanceTimersByTimeAsync(150);
      defaultRequest.resolve(makePage('default history'));
      await Promise.resolve();
    });
    await act(async () => {
      queryRequest.reject(new Error('synthetic query rejection'));
      await Promise.resolve();
    });

    expect(result.current.query).toBe('');
    expect(result.current.status).toBe('ready');
    expect(result.current.items[0]?.preview).toBe('default history');
    expect(result.current).not.toHaveProperty('error');
  });

  it('ignores an older rejection after the gateway is replaced', async () => {
    const firstRequest = deferred<HistoryPage>();
    const secondRequest = deferred<HistoryPage>();
    const firstGateway = gatewayWithSearch(() => firstRequest.promise);
    const secondGateway = gatewayWithSearch(() => secondRequest.promise);
    const { result, rerender } = renderHook(
      ({ gateway }) => useHistorySearch(gateway),
      { initialProps: { gateway: firstGateway } },
    );

    await act(async () => {
      await vi.advanceTimersByTimeAsync(150);
    });
    rerender({ gateway: secondGateway });
    await act(async () => {
      await vi.advanceTimersByTimeAsync(150);
      secondRequest.resolve(makePage('replacement gateway'));
      await Promise.resolve();
    });
    await act(async () => {
      firstRequest.reject(new Error('synthetic old gateway rejection'));
      await Promise.resolve();
    });

    expect(result.current.status).toBe('ready');
    expect(result.current.items[0]?.preview).toBe('replacement gateway');
    expect(result.current).not.toHaveProperty('error');
  });

  it('exposes a generic error state without returning the rejected error', async () => {
    const gateway = gatewayWithSearch(() =>
      Promise.reject(new Error('/private/export.json: secret query failed')),
    );
    const { result } = renderHook(() => useHistorySearch(gateway));

    await act(async () => {
      await vi.advanceTimersByTimeAsync(150);
      await Promise.resolve();
    });

    expect(result.current.status).toBe('error');
    expect(result.current.items).toEqual([]);
    expect(result.current).not.toHaveProperty('error');
  });
});
