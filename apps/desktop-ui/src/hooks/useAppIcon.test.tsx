import { renderHook, waitFor } from '@testing-library/react';
import { describe, expect, it, vi } from 'vitest';

import type { Thumbnail } from '../lib/contracts';
import type { ClipboardGateway } from '../lib/gateway';
import { useAppIcon } from './useAppIcon';

/// One fresh path per test: the module-level cache is the point of this
/// hook, and a shared path would let one test answer another's assertion.
const freshPath = (): string => `/synthetic/Applications/${crypto.randomUUID()}/App.app`;

const syntheticIcon: Thumbnail = {
  mimeType: 'image/png',
  base64:
    'iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKsMIgAAAABJRU5ErkJggg==',
};

const makeGateway = (
  getAppIcon: ClipboardGateway['getAppIcon'],
): ClipboardGateway => ({ getAppIcon }) as unknown as ClipboardGateway;

describe('useAppIcon', () => {
  it('turns a fetched icon into a data URL', async () => {
    const path = freshPath();
    const gateway = makeGateway(vi.fn(async () => syntheticIcon));

    const { result } = renderHook(() => useAppIcon(gateway, path));

    await waitFor(() => expect(result.current.status).toBe('ready'));
    expect(result.current.url).toMatch(/^data:image\/png;base64,/);
  });

  it('answers the second mount of the same path from the cache', async () => {
    const path = freshPath();
    const getAppIcon = vi.fn(async () => syntheticIcon);
    const gateway = makeGateway(getAppIcon);

    const first = renderHook(() => useAppIcon(gateway, path));
    await waitFor(() => expect(first.result.current.status).toBe('ready'));
    first.unmount();

    const second = renderHook(() => useAppIcon(gateway, path));
    await waitFor(() => expect(second.result.current.status).toBe('ready'));
    expect(second.result.current.url).toMatch(/^data:image\/png;base64,/);

    expect(getAppIcon).toHaveBeenCalledOnce();
  });

  it('settles on the glyph when there is no icon to draw', async () => {
    const path = freshPath();
    const gateway = makeGateway(vi.fn(async () => null));

    const { result } = renderHook(() => useAppIcon(gateway, path));

    await waitFor(() => expect(result.current.status).toBe('ready'));
    expect(result.current.url).toBeNull();
  });

  it('survives a rejected fetch and settles on the glyph', async () => {
    const path = freshPath();
    const gateway = makeGateway(
      vi.fn(async () => {
        throw new Error('icon_unavailable');
      }),
    );

    const { result } = renderHook(() => useAppIcon(gateway, path));

    await waitFor(() => expect(result.current.status).toBe('ready'));
    expect(result.current.url).toBeNull();
  });
});
