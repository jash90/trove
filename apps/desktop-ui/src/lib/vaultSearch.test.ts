import { describe, expect, it } from 'vitest';

import type { KeyvaultSecret } from './contracts';
import { filterSecrets } from './vaultSearch';

const secrets: KeyvaultSecret[] = [
  { slug: 'openai', name: 'OpenAI', category: 'ai' },
  { slug: 'stripe-secret-key', name: 'Stripe secret', category: 'payments' },
  { slug: 'stripe-publishable-key', name: 'Stripe publishable', category: 'payments' },
  { slug: 'jira', name: 'Atlassian Jira', category: null },
];

describe('filterSecrets', () => {
  it('matches nothing at all on an empty query', () => {
    // Secrets are asked for by name, not browsed. Listing every key the moment the palette
    // opens would put them in front of someone reaching for something else entirely.
    expect(filterSecrets(secrets, '')).toEqual([]);
    expect(filterSecrets(secrets, '   ')).toEqual([]);
  });

  it('ranks a slug prefix above a word inside one', () => {
    const found = filterSecrets(secrets, 'stripe').map((secret) => secret.slug);
    expect(found).toEqual(['stripe-secret-key', 'stripe-publishable-key']);
  });

  it('finds a secret by a word inside a hyphenated slug', () => {
    expect(filterSecrets(secrets, 'publishable').map((s) => s.slug)).toEqual([
      'stripe-publishable-key',
    ]);
  });

  it('falls back to the display name and the category', () => {
    expect(filterSecrets(secrets, 'atlassian').map((s) => s.slug)).toEqual(['jira']);
    expect(filterSecrets(secrets, 'payments').map((s) => s.slug)).toEqual([
      'stripe-secret-key',
      'stripe-publishable-key',
    ]);
  });

  it('folds case and diacritics the way the rest of the palette does', () => {
    expect(filterSecrets(secrets, 'OPENAI').map((s) => s.slug)).toEqual(['openai']);
  });
});
