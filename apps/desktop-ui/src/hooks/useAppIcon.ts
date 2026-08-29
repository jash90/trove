import { useEffect, useState } from 'react';

import type { Thumbnail } from '../lib/contracts';
import { thumbnailDataUrl } from '../lib/format';
import type { ClipboardGateway } from '../lib/gateway';

export type AppIconStatus = 'loading' | 'ready';

export interface UseAppIconResult {
  status: AppIconStatus;
  /** The data URL of the rendered icon, or null when the row keeps its glyph. */
  url: string | null;
}

/// Rendered icons remembered for the session, keyed by catalog path. An
/// icon is a fact about a bundle that changes only with the bundle, and the
/// virtualized list remounts rows as the user scrolls — without this map,
/// every scroll past an application would ask the core for pixels it
/// already sent. The backend holds the same memory; this one keeps the
/// remount from even asking.
const iconCache = new Map<string, string | null>();

/**
 * One application's rendered icon, fetched once per path per session.
 *
 * `null` is an answer like any other — no icon to draw, or a fetch that
 * failed — and it is remembered too: a broken icon re-asked on every scroll
 * is a chatty kind of giving up. The row falls back to its glyph and the
 * next palette entry (fresh catalog) gets a fresh chance.
 */
export const useAppIcon = (
  gateway: ClipboardGateway,
  path: string,
): UseAppIconResult => {
  const [state, setState] = useState<UseAppIconResult>(() => ({
    status: 'ready',
    url: iconCache.get(path) ?? null,
  }));

  useEffect(() => {
    if (iconCache.has(path)) {
      setState({ status: 'ready', url: iconCache.get(path) ?? null });
      return;
    }
    setState({ status: 'loading', url: null });
    let active = true;
    void gateway
      .getAppIcon(path)
      .then((icon: Thumbnail | null) => (icon === null ? null : thumbnailDataUrl(icon)))
      .then((url) => {
        if (!active) return;
        iconCache.set(path, url);
        setState({ status: 'ready', url });
      })
      .catch(() => {
        if (!active) return;
        iconCache.set(path, null);
        setState({ status: 'ready', url: null });
      });
    return () => {
      active = false;
    };
  }, [gateway, path]);

  return state;
};
