import { useEffect, useRef, useState } from 'react';

import type { KeyvaultSecret } from '../lib/contracts';
import type { ClipboardGateway } from '../lib/gateway';

export interface UseVaultCatalogResult {
  secrets: KeyvaultSecret[];
}

/**
 * The vault's secret metadata, fetched once and then searched locally.
 *
 * Shaped like `useAppsCatalog`, with two deliberate differences.
 *
 * It is not always enabled. The application catalog scans the local disk, so it may load the
 * moment the palette opens; this one crosses the network to someone's vault, and an application
 * that promises to stay off the network until it is used has to mean it. So `enabled` is driven
 * by the query being non-empty, and an untouched palette asks nothing.
 *
 * It does not refetch when the query changes. The list is metadata — names, not values — so it
 * is fetched once and filtered in memory. Refetching per keystroke would spend the vault's
 * per-token rate limit within a few characters and return refusals instead of results.
 *
 * A vault that is not configured is silence, not an error: most installs have none, and every
 * one of them would otherwise see a failure notice for a feature they never set up.
 */
export const useVaultCatalog = (
  gateway: ClipboardGateway,
  enabled: boolean,
): UseVaultCatalogResult => {
  const [secrets, setSecrets] = useState<KeyvaultSecret[]>([]);
  const requestId = useRef(0);
  const loaded = useRef(false);

  useEffect(() => {
    if (!enabled || loaded.current) return;
    const id = ++requestId.current;
    // Marked before the answer arrives: two keystrokes in flight must not become two requests,
    // which is the whole reason this is fetched once rather than per query.
    loaded.current = true;

    void gateway
      .keyvaultList()
      .then((list) => {
        if (id === requestId.current) setSecrets(list);
      })
      .catch(() => {
        if (id !== requestId.current) return;
        // Includes keyvault_not_configured, which is the ordinary state of an install with no
        // vault. Contributing nothing is the correct outcome for every failure here: the palette
        // still has history and applications to show, and a key nobody can reach is not an
        // emergency worth a banner over someone else's search.
        setSecrets([]);
      });
  }, [gateway, enabled]);

  return { secrets };
};
