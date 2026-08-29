import '@testing-library/jest-dom/vitest';
import { render, screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { App } from './App';
import { mockGateway, type ClipboardGateway } from './lib/gateway';
import { SYNTHETIC_APPS } from './lib/fixtures';
import type { AppEntry } from './lib/contracts';

it('renders the private clipboard palette landmark', () => {
  render(<App />);
  expect(screen.getByRole('application', { name: 'Clipboard history' })).toBeVisible();
});

it('opens the import wizard over the palette and returns focus to the search field', async () => {
  const user = userEvent.setup();
  render(<App />);
  const search = screen.getByRole('searchbox', { name: 'Search history' });

  await user.click(screen.getByRole('button', { name: 'Importuj archiwum' }));
  expect(screen.getByRole('dialog', { name: 'Import history' })).toBeVisible();
  expect(screen.getByLabelText('Clipboard history palette')).toHaveAttribute('inert');

  await user.click(screen.getByRole('button', { name: 'Zamknij import' }));
  // The palette has one place a keyboard user works from, and a shortcut has
  // no button to hand focus back to.
  await waitFor(() => expect(search).toHaveFocus());
  expect(screen.getByLabelText('Clipboard history palette')).not.toHaveAttribute('inert');
});

it('asks for the settings window rather than covering the list with a dialog', async () => {
  const user = userEvent.setup();
  const openSettingsWindow = vi.fn(async () => undefined);
  render(<App gateway={{ ...mockGateway, openSettingsWindow }} />);

  await user.click(screen.getByRole('button', { name: 'Open settings' }));
  expect(openSettingsWindow).toHaveBeenCalledOnce();

  await user.keyboard('{Meta>},{/Meta}');
  expect(openSettingsWindow).toHaveBeenCalledTimes(2);
  // Nothing was drawn over the palette either way.
  expect(screen.queryByRole('dialog', { name: 'Settings' })).not.toBeInTheDocument();
});

it('opens the import wizard from the keyboard, with no history selected', async () => {
  const user = userEvent.setup();
  render(<App />);

  await user.keyboard('{Meta>}i{/Meta}');
  expect(await screen.findByRole('dialog', { name: 'Import history' })).toBeVisible();
});

describe('the launcher mode', () => {
  beforeEach(() => {
    // jsdom reports zero-sized elements, so the virtualizer would render no rows.
    vi.spyOn(HTMLElement.prototype, 'offsetWidth', 'get').mockReturnValue(900);
    vi.spyOn(HTMLElement.prototype, 'offsetHeight', 'get').mockReturnValue(480);
  });

  afterEach(() => {
    vi.restoreAllMocks();
  });

  const historyResults = () =>
    within(screen.getByRole('listbox', { name: 'Clipboard history results' }));
  const appsResults = () =>
    within(screen.getByRole('listbox', { name: 'Application results' }));

  const settleHistory = async (): Promise<void> => {
    await waitFor(() =>
      expect(historyResults().getAllByRole('option').length).toBeGreaterThan(0),
    );
  };
  const settleApps = async (): Promise<void> => {
    await waitFor(() =>
      expect(appsResults().getAllByRole('option').length).toBeGreaterThan(0),
    );
  };

  it('tab switches between history and applications and back, keeping the field focused', async () => {
    const user = userEvent.setup();
    render(<App />);
    await settleHistory();
    const search = screen.getByRole('searchbox');

    await user.keyboard('{Tab}');
    await settleApps();
    expect(screen.queryByRole('listbox', { name: 'Clipboard history results' })).toBeNull();
    // The landmark follows the mode: a screen reader entering it deserves
    // the context the inner labels already carry.
    expect(screen.getByRole('application', { name: 'Application launcher' })).toBeVisible();
    // The field is shared, not remounted: focus survives the toggle.
    expect(search).toHaveFocus();

    await user.keyboard('{Tab}');
    await settleHistory();
    expect(screen.queryByRole('listbox', { name: 'Application results' })).toBeNull();
    expect(screen.getByRole('application', { name: 'Clipboard history' })).toBeVisible();
    expect(search).toHaveFocus();
  });

  it('typing in the mode filters the catalog without refetching it', async () => {
    const user = userEvent.setup();
    const listApps = vi.fn(async () => SYNTHETIC_APPS.map((app) => ({ ...app })));
    render(<App gateway={{ ...mockGateway, listApps } as ClipboardGateway} />);

    await settleHistory();
    await user.keyboard('{Tab}');
    await settleApps();
    expect(listApps).toHaveBeenCalledOnce();

    await user.type(screen.getByRole('searchbox'), 'notes');
    await waitFor(() => expect(appsResults().getAllByRole('option')).toHaveLength(1));
    expect(appsResults().getByRole('option', { selected: true })).toHaveTextContent(
      'Synthetic Notes',
    );

    // Every keystroke after the first narrowed a list already on the client.
    expect(listApps).toHaveBeenCalledOnce();
  });

  it('enter launches the selected application by its catalog path', async () => {
    const user = userEvent.setup();
    const launchApp = vi.fn(async () => undefined);
    render(<App gateway={{ ...mockGateway, launchApp } as ClipboardGateway} />);

    await settleHistory();
    await user.keyboard('{Tab}');
    await settleApps();

    await user.type(screen.getByRole('searchbox'), 'terminal');
    await waitFor(() => expect(appsResults().getAllByRole('option')).toHaveLength(1));
    await user.keyboard('{Enter}');

    expect(launchApp).toHaveBeenCalledWith(
      '/synthetic/Applications/Utilities/Synthetic Terminal.app',
    );
  });

  it('escape clears the query before leaving the mode, and never closes the palette', async () => {
    const user = userEvent.setup();
    render(<App />);

    await settleHistory();
    await user.keyboard('{Tab}');
    await settleApps();
    await user.type(screen.getByRole('searchbox'), 'notes');

    await user.keyboard('{Escape}');
    expect(screen.getByRole('searchbox')).toHaveValue('');
    expect(appsResults().getAllByRole('option').length).toBeGreaterThan(0);

    await user.keyboard('{Escape}');
    await settleHistory();
    expect(screen.getByRole('application', { name: 'Clipboard history' })).toBeVisible();
  });

  it('keeps the row shortcuts of the history mode inert here', async () => {
    const user = userEvent.setup();
    const setPinned = vi.fn(async () => undefined);
    const deleteEvent = vi.fn(async () => undefined);
    render(
      <App gateway={{ ...mockGateway, setPinned, deleteEvent } as ClipboardGateway} />,
    );

    await settleHistory();
    await user.keyboard('{Tab}');
    await settleApps();

    await user.keyboard('{Meta>}p{/Meta}');
    await user.keyboard('{Delete}');
    await user.keyboard('{Meta>}c{/Meta}');

    expect(setPinned).not.toHaveBeenCalled();
    expect(deleteEvent).not.toHaveBeenCalled();
    expect(screen.queryByRole('dialog')).toBeNull();
  });

  it('reports a failed launch without repeating the path it was given', async () => {
    const user = userEvent.setup();
    const launchApp = vi.fn(async () => {
      throw new Error('/private/wherever/the/app/was.app');
    });
    render(<App gateway={{ ...mockGateway, launchApp } as ClipboardGateway} />);

    await settleHistory();
    await user.keyboard('{Tab}');
    await settleApps();

    await user.type(screen.getByRole('searchbox'), 'terminal');
    await waitFor(() => expect(appsResults().getAllByRole('option')).toHaveLength(1));
    await user.keyboard('{Enter}');

    const alert = await screen.findByRole('alert');
    expect(alert).toHaveTextContent(/could not be started/i);
    expect(alert).not.toHaveTextContent('/private');
    expect(alert).not.toHaveTextContent('.app');
  });

  it('keeps the fetched list on screen while the catalog reloads', async () => {
    const user = userEvent.setup();
    // The first answer arrives; the second — the re-entry refetch — hangs,
    // which is exactly the window a loading state used to swallow the list in.
    const listApps = vi.fn<(typeof mockGateway)['listApps']>()
      .mockImplementationOnce(async () => SYNTHETIC_APPS.map((app) => ({ ...app })))
      .mockImplementationOnce(
        () => new Promise<AppEntry[]>(() => undefined),
      );
    render(<App gateway={{ ...mockGateway, listApps } as ClipboardGateway} />);

    await settleHistory();
    await user.keyboard('{Tab}');
    await settleApps();
    expect(appsResults().getAllByRole('option').length).toBeGreaterThan(0);

    await user.keyboard('{Tab}');
    await user.keyboard('{Tab}');

    // The reload is in flight and the list never left the screen, so Enter
    // can only act on applications the user can actually see.
    expect(appsResults().getAllByRole('option').length).toBeGreaterThan(0);
    expect(listApps).toHaveBeenCalledTimes(2);
  });

  it('leaves Tab to the dialog while one is open', async () => {
    const user = userEvent.setup();
    render(<App />);
    await settleHistory();

    await user.keyboard('{Meta>}i{/Meta}');
    expect(await screen.findByRole('dialog', { name: 'Import history' })).toBeVisible();

    await user.keyboard('{Tab}');

    // The guard, not the inert section, is what stops the toggle — and the
    // dialog keeps Tab for its own controls either way.
    expect(screen.getByRole('listbox', { name: 'Clipboard history results' })).toBeInTheDocument();
    expect(screen.queryByRole('listbox', { name: 'Application results' })).toBeNull();
    expect(screen.getByRole('dialog', { name: 'Import history' })).toBeVisible();
  });
});
