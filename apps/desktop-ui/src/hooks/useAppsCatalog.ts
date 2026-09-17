import { getCurrentWindow } from '@tauri-apps/api/window';
import { useCallback, useEffect, useRef, useState } from 'react';

import type { AppEntry } from '../lib/contracts';
import type { ClipboardGateway } from '../lib/gateway';

export type AppsCatalogStatus = 'loading' | 'ready' | 'error';

export interface UseAppsCatalogResult {
  status: AppsCatalogStatus;
  apps: AppEntry[];
}

/**
 * The launcher's application catalog.
 *
 * Asked when the palette mounts and re-asked every time it is summoned: the
 * window is hidden rather than unmounted, so a focus change is the moment a
 * new opening actually happens, and a rescan that starts there is what keeps
 * a freshly installed application from staying invisible until the next
 * launch of the whole application.
 *
 * A refetch never costs the screen its rows: the previous catalog stays
 * while the answer is on its way, and a failed refresh keeps it too — the
 * list only ever swaps, whole, when a fresh one arrived. The backend answers
 * from its cache immediately and rescans beside it, so the round trip is
 * short even when the disk walk is not.
 *
 * The core also pushes `onAppsChanged` when its background scan noticed
 * something new, so the catalog updates while the palette is open rather
 * than at the next summoning.
 *
 * A late answer that arrives after the mode closed is dropped, not applied:
 * the user has already moved on, and the next entry refetches anyway.
 */
export const useAppsCatalog = (
  gateway: ClipboardGateway,
  enabled: boolean,
): UseAppsCatalogResult => {
  const [state, setState] = useState<UseAppsCatalogResult>({
    status: 'loading',
    apps: [],
  });
  const requestId = useRef(0);

  const fetchCatalog = useCallback((): void => {
    const id = ++requestId.current;
    void gateway
      .listApps()
      .then((apps) => {
        if (id === requestId.current) {
          setState({ status: 'ready', apps });
        }
      })
      .catch(() => {
        if (id !== requestId.current) return;
        setState((previous) =>
          previous.apps.length > 0
            ? previous
            : { status: 'error', apps: [] },
        );
      });
  }, [gateway]);

  useEffect(() => {
    if (!enabled) {
      // Invalidate whatever is still in flight: the mode is closed, and its
      // answer belongs to a view nobody is looking at.
      requestId.current++;
      return;
    }
    fetchCatalog();
  }, [enabled, fetchCatalog]);

  // Every summoning re-asks: the palette is hidden and shown, never
  // remounted, so the focus event is the only signal a new opening is.
  useEffect(() => {
    if (!enabled) return;
    let stop: (() => void) | null = null;
    let cancelled = false;
    // try/catch around the call itself, not only the promise: outside a
    // Tauri window getCurrentWindow throws where it stands.
    try {
      void getCurrentWindow()
        .onFocusChanged(({ payload: focused }) => {
          if (focused) fetchCatalog();
        })
        .then((unlisten) => {
          if (cancelled) unlisten();
          else stop = unlisten;
        })
        .catch(() => undefined);
    } catch {
      /* no window to listen to; the browser preview refetches on mount only */
    }
    return () => {
      cancelled = true;
      stop?.();
    };
  }, [enabled, fetchCatalog]);

  // The background scan's verdict, delivered while the palette is open.
  useEffect(() => {
    if (!enabled || gateway.onAppsChanged === undefined) return;
    return gateway.onAppsChanged(fetchCatalog);
  }, [enabled, gateway, fetchCatalog]);

  return state;
};
