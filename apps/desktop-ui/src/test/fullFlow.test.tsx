import '@testing-library/jest-dom/vitest';
import { render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import axe from 'axe-core';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import { App } from '../App';
import { SYNTHETIC_HISTORY_ITEMS } from '../lib/fixtures';
import { mockGateway } from '../lib/mockGateway';

/**
 * These run the real components against the same synthetic gateway the browser
 * preview uses. Nothing here touches a database, a native command, or any real
 * clipboard history: the fixtures are invented.
 */

const settle = async (): Promise<void> => {
  await waitFor(() => expect(screen.getAllByRole('option').length).toBeGreaterThan(0), {
    timeout: 2_000,
  });
};

describe('the palette end to end', () => {
  // jsdom reports zero-sized elements, so the virtualizer would render no rows.
  beforeEach(() => {
    vi.spyOn(HTMLElement.prototype, 'offsetWidth', 'get').mockReturnValue(900);
    vi.spyOn(HTMLElement.prototype, 'offsetHeight', 'get').mockReturnValue(480);
  });

  afterEach(() => {
    vi.restoreAllMocks();
  });

  it('searches, selects, previews, and pins without leaking payload bytes', async () => {
    const user = userEvent.setup();
    const { container } = render(<App gateway={mockGateway} />);
    await settle();

    await user.type(screen.getByRole('searchbox'), 'project note');
    await waitFor(() => expect(screen.getAllByRole('option')).toHaveLength(1));

    await user.keyboard('{ArrowDown}');
    const selected = screen.getByRole('option', { selected: true });
    expect(selected).toHaveAttribute('data-event-id', String(SYNTHETIC_HISTORY_ITEMS[0].eventId));

    await user.keyboard('{Meta>}p{/Meta}');
    await waitFor(() =>
      expect(screen.getByRole('button', { name: /przypnij wpis|odepnij wpis/i })).toBeVisible(),
    );

    // The list carries previews and metadata, never representation bytes.
    expect(container.innerHTML).not.toContain('inlinePayload');
    expect(container.innerHTML).not.toContain('blobRelpath');
  });

  it('shows a file entry with its source location and no reveal action when it is gone', async () => {
    render(<App gateway={mockGateway} />);
    await settle();

    const user = userEvent.setup();
    await user.click(screen.getByRole('option', { name: /raport-syntetyczny\.pdf/ }));

    expect(await screen.findByText('Lokalizacja źródłowa')).toBeVisible();
    expect(screen.getByText('Plik nie istnieje')).toBeVisible();
    expect(screen.queryByRole('button', { name: 'Pokaż w Finderze' })).toBeNull();
  });

  it('opens the import wizard and the settings workspace from the header', async () => {
    const user = userEvent.setup();
    render(<App gateway={mockGateway} />);
    await settle();

    await user.click(screen.getByRole('button', { name: 'Importuj archiwum' }));
    expect(screen.getByRole('dialog', { name: 'Importuj historię' })).toBeVisible();
    await user.click(screen.getByRole('button', { name: 'Zamknij import' }));

    await user.click(screen.getByRole('button', { name: 'Otwórz ustawienia' }));
    expect(await screen.findByRole('dialog', { name: 'Ustawienia' })).toBeVisible();
    // Retention is off by default, so history stays unbounded.
    expect(screen.getByRole('checkbox', { name: 'Bez limitu retencji' })).toBeChecked();
    await user.click(screen.getByRole('button', { name: 'Zamknij ustawienia' }));
  });

  it('has no serious automated accessibility violations', async () => {
    const { container } = render(<App gateway={mockGateway} />);
    await settle();

    const result = await axe.run(container);
    const serious = result.violations.filter((violation) =>
      ['serious', 'critical'].includes(violation.impact ?? ''),
    );

    expect(serious.map((violation) => violation.id)).toEqual([]);
  });
});
