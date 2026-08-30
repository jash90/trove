import { renderHook, waitFor } from '@testing-library/react';
import { describe, expect, it, vi } from 'vitest';

import type { KeyvaultSecret } from '../lib/contracts';
import type { ClipboardGateway } from '../lib/gateway';
import { useVaultCatalog } from './useVaultCatalog';

const syntheticSecrets: KeyvaultSecret[] = [
  { slug: 'openai', name: 'OpenAI', category: 'ai' },
  { slug: 'stripe-secret-key', name: 'Stripe secret key', category: 'payments' },
];

const makeGateway = (
  keyvaultList: ClipboardGateway['keyvaultList'],
): ClipboardGateway => ({ keyvaultList }) as unknown as ClipboardGateway;

describe('useVaultCatalog', () => {
  it('asks nothing while the palette is untouched', async () => {
    const keyvaultList = vi.fn(async () => syntheticSecrets);
    renderHook(() => useVaultCatalog(makeGateway(keyvaultList), false));

    // The promise this application makes is that it stays off the network until it is used, and
    // an empty palette has not been used. Nothing else in this hook matters if this is wrong.
    await new Promise((resolve) => setTimeout(resolve, 20));
    expect(keyvaultList).not.toHaveBeenCalled();
  });

  it('fetches once however much is typed afterwards', async () => {
    const keyvaultList = vi.fn(async () => syntheticSecrets);
    const gateway = makeGateway(keyvaultList);
    const { result, rerender } = renderHook(() => useVaultCatalog(gateway, true));

    await waitFor(() => expect(result.current.secrets).toHaveLength(2));

    // Every keystroke re-renders. Refetching on each would spend the vault's per-token rate
    // limit within a few characters and answer a search with refusals.
    rerender();
    rerender();
    rerender();
    expect(keyvaultList).toHaveBeenCalledOnce();
  });

  it('treats a vault that is not configured as silence, not as a failure', async () => {
    const keyvaultList = vi.fn(async () => {
      throw new Error('keyvault_not_configured');
    });
    const { result } = renderHook(() =>
      useVaultCatalog(makeGateway(keyvaultList), true),
    );

    await waitFor(() => expect(keyvaultList).toHaveBeenCalledOnce());
    // Most installs have no vault. Contributing nothing is right for every failure here — the
    // palette still has history and applications, and there is no banner to put over someone
    // else's search.
    expect(result.current.secrets).toEqual([]);
  });
});
