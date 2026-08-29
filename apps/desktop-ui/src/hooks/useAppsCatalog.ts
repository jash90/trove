import { useEffect, useRef, useState } from 'react';

import type { AppEntry } from '../lib/contracts';
import type { ClipboardGateway } from '../lib/gateway';

export type AppsCatalogStatus = 'loading' | 'ready' | 'error';

export interface UseAppsCatalogResult {
  status: AppsCatalogStatus;
  apps: AppEntry[];
}

/**
 * The launcher's application catalog, fetched once per mode entry.
 *
 * The palette filters as the user types, so this hook never refetches on a
 * query change — only when the mode is entered again, when the backend's
 * TTL-cached scan decides how much work that costs. `enabled` is a parameter
 * rather than a mount condition because the palette component stays mounted
 * across mode switches and so should the catalog it already loaded.
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

  useEffect(() => {
    if (!enabled) {
      // Invalidate whatever is still in flight: the mode is closed, and its
      // answer belongs to a view nobody is looking at.
      requestId.current++;
      return;
    }
    const id = ++requestId.current;
    setState((previous) => ({ ...previous, status: 'loading' }));

    void gateway
      .listApps()
      .then((apps) => {
        if (id === requestId.current) {
          setState({ status: 'ready', apps });
        }
      })
      .catch(() => {
        if (id === requestId.current) {
          setState({ status: 'error', apps: [] });
        }
      });
  }, [gateway, enabled]);

  return state;
};
