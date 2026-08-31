import '@testing-library/jest-dom/vitest';
import { render, screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import axe from 'axe-core';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import { App } from '../App';
import { SYNTHETIC_HISTORY_ITEMS } from '../lib/fixtures';
import { mockGateway } from '../lib/mockGateway';

/// The history rows, and only those.
///
/// The type filter beside the search field is a combobox, and its choices are
/// options too. An unscoped option query matches both, so a test can pass
/// while the list it meant to inspect has not loaded at all.
const historyList = () =>
  within(screen.getByRole('listbox', { name: 'Applications, secrets and history results' }));


/**
 * These run the real components against the same synthetic gateway the browser
 * preview uses. Nothing here touches a database, a native command, or any real
 * clipboard history: the fixtures are invented.
 */

const settle = async (): Promise<void> => {
  await waitFor(() => expect(historyList().getAllByRole('option').length).toBeGreaterThan(0), {
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
    await waitFor(() => expect(historyList().getAllByRole('option')).toHaveLength(1));

    await user.keyboard('{ArrowDown}');
    const selected = historyList().getByRole('option', { selected: true });
    expect(selected).toHaveAttribute('data-event-id', String(SYNTHETIC_HISTORY_ITEMS[0].eventId));

    await user.keyboard('{Meta>}p{/Meta}');
    await waitFor(() =>
      expect(screen.getByRole('button', { name: /pin entry|unpin entry/i })).toBeVisible(),
    );

    // The list carries previews and metadata, never representation bytes.
    expect(container.innerHTML).not.toContain('inlinePayload');
    expect(container.innerHTML).not.toContain('blobRelpath');
  });

  it('shows a file entry with its source location', async () => {
    render(<App gateway={mockGateway} />);
    await settle();

    const user = userEvent.setup();
    // The applications sit above the history now; a query only the file
    // entry matches brings its row into the virtualized window.
    await user.type(screen.getByRole('searchbox'), 'raport');
    await waitFor(() =>
      expect(historyList().getByRole('option', { name: /raport-syntetyczny\.pdf/ })).toBeVisible(),
    );
    await user.click(historyList().getByRole('option', { name: /raport-syntetyczny\.pdf/ }));

    expect(await screen.findByText('Source location')).toBeVisible();
    expect(
      screen.getByText('/synthetic/archiwum/notatka-syntetyczna.pdf'),
    ).toBeVisible();
  });

  it('opens the import wizard here and asks for the settings window elsewhere', async () => {
    const user = userEvent.setup();
    const openSettingsWindow = vi.fn(async () => undefined);
    render(<App gateway={{ ...mockGateway, openSettingsWindow }} />);
    await settle();

    await user.click(screen.getByRole('button', { name: 'Import an archive' }));
    expect(screen.getByRole('dialog', { name: 'Import history' })).toBeVisible();
    await user.click(screen.getByRole('button', { name: 'Close import' }));

    // Settings are their own OS window, so the palette asks for it and keeps
    // showing the list rather than covering it.
    await user.click(screen.getByRole('button', { name: 'Open settings' }));
    expect(openSettingsWindow).toHaveBeenCalledOnce();
    expect(screen.queryByRole('dialog', { name: 'Settings' })).not.toBeInTheDocument();
    expect(historyList().getAllByRole('option').length).toBeGreaterThan(0);
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

  it('has no accessibility violations with both sections on screen', async () => {
    const { container } = render(<App gateway={mockGateway} />);
    await settle();

    await waitFor(() =>
      expect(
        within(screen.getByRole('listbox', { name: 'Applications, secrets and history results' })).getAllByRole(
          'option',
        ).length,
      ).toBeGreaterThan(0),
    );

    const result = await axe.run(container);
    const serious = result.violations.filter((violation) =>
      ['serious', 'critical'].includes(violation.impact ?? ''),
    );

    expect(serious.map((violation) => violation.id)).toEqual([]);
  });
});
