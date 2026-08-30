import '@testing-library/jest-dom/vitest';
import { render, screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { App } from './App';
import { mockGateway, type ClipboardGateway } from './lib/gateway';
import { SYNTHETIC_APPS } from './lib/fixtures';

it('renders the private clipboard palette landmark', () => {
  render(<App />);
  expect(screen.getByRole('application', { name: 'Clipboard palette' })).toBeVisible();
});

it('opens the import wizard over the palette and returns focus to the search field', async () => {
  const user = userEvent.setup();
  render(<App />);
  const search = screen.getByRole('searchbox');

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

describe('the unified palette', () => {
  beforeEach(() => {
    // jsdom reports zero-sized elements, so the virtualizer would render no rows.
    vi.spyOn(HTMLElement.prototype, 'offsetWidth', 'get').mockReturnValue(900);
    vi.spyOn(HTMLElement.prototype, 'offsetHeight', 'get').mockReturnValue(480);
  });

  afterEach(() => {
    vi.restoreAllMocks();
  });

  const historyResults = () =>
    within(screen.getByRole('listbox', { name: 'Application and history results' }));
  const appsResults = () =>
    within(screen.getByRole('listbox', { name: 'Application and history results' }));

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

  it('shows applications above the history before anything is typed', async () => {
    render(<App />);
    await settleApps();
    await settleHistory();

    // A launcher from the first frame: the whole catalog, alphabetically,
    // with the recent history underneath it — one list, apps first.
    await waitFor(() =>
      expect(
        historyResults()
          .getAllByRole('option')
          .some((option) => option.hasAttribute('data-event-id')),
      ).toBe(true),
    );
    const options = historyResults().getAllByRole('option');
    const appRows = options.filter((option) => option.hasAttribute('data-path'));
    const firstHistoryRow = options.findIndex((option) =>
      option.hasAttribute('data-event-id'),
    );
    expect(appRows).toHaveLength(SYNTHETIC_APPS.length);
    expect(firstHistoryRow).toBe(appRows.length);
  });

  it('filters applications client-side while typing once fetched', async () => {
    const user = userEvent.setup();
    const listApps = vi.fn(async () => SYNTHETIC_APPS.map((app) => ({ ...app })));
    render(<App gateway={{ ...mockGateway, listApps } as ClipboardGateway} />);

    await settleApps();
    await settleHistory();

    await user.type(screen.getByRole('searchbox'), 'notes');
    await waitFor(() => expect(appsResults().getAllByRole('option')).toHaveLength(1));
    expect(appsResults().getByRole('option', { selected: true })).toHaveTextContent(
      'Synthetic Notes',
    );

    // Every keystroke after the first narrowed a list already on the client.
    expect(listApps).toHaveBeenCalledOnce();
  });

  it('launches the selected application with Enter', async () => {
    const user = userEvent.setup();
    const launchApp = vi.fn(async () => undefined);
    render(<App gateway={{ ...mockGateway, launchApp } as ClipboardGateway} />);

    await settleApps();
    await settleHistory();

    await user.type(screen.getByRole('searchbox'), 'terminal');
    await waitFor(() => expect(appsResults().getAllByRole('option')).toHaveLength(1));
    await user.keyboard('{Enter}');

    expect(launchApp).toHaveBeenCalledWith(
      '/synthetic/Applications/Utilities/Synthetic Terminal.app',
    );
  });

  it('pastes the selected history entry when no application matches', async () => {
    const user = userEvent.setup();
    const copyEvent = vi.fn(async () => ({ mode: 'pasted', plainText: false }));
    render(<App gateway={{ ...mockGateway, copyEvent } as ClipboardGateway} />);

    await settleApps();
    await settleHistory();

    // A query no application matches: every row left is history, so the
    // first selectable one pastes.
    await user.type(screen.getByRole('searchbox'), 'project note');
    await waitFor(() =>
      expect(
        historyResults()
          .getAllByRole('option')
          .filter((option) => option.hasAttribute('data-path')),
      ).toHaveLength(0),
    );
    await waitFor(() =>
      expect(historyResults().getAllByRole('option').length).toBeGreaterThan(0),
    );

    await user.keyboard('{Enter}');

    expect(copyEvent).toHaveBeenCalledWith(expect.any(Number), false, true);
  });

  it('opens the preview only for a selected history entry', async () => {
    const user = userEvent.setup();
    render(<App />);

    await settleApps();
    await settleHistory();

    // An application holds the selection by default; an application has no
    // payload to preview, so the pane shows nothing of one.
    expect(screen.queryByText('Synthetic project note for browser preview')).toBeNull();

    // A query no application matches makes a history row the selection, and
    // that is what the pane opens for.
    await user.type(screen.getByRole('searchbox'), 'project note');
    await waitFor(() =>
      expect(screen.getByText('Synthetic project note for browser preview')).toBeVisible(),
    );
  });

  it('keeps the row shortcuts of the history inert while an application is selected', async () => {
    const user = userEvent.setup();
    const setPinned = vi.fn(async () => undefined);
    const deleteEvent = vi.fn(async () => undefined);
    render(
      <App gateway={{ ...mockGateway, setPinned, deleteEvent } as ClipboardGateway} />,
    );

    await settleApps();
    await settleHistory();

    await user.type(screen.getByRole('searchbox'), 'terminal');
    await waitFor(() => expect(appsResults().getAllByRole('option')).toHaveLength(1));

    await user.keyboard('{Meta>}p{/Meta}');
    await user.keyboard('{Delete}');
    await user.keyboard('{Meta>}c{/Meta}');

    expect(setPinned).not.toHaveBeenCalled();
    expect(deleteEvent).not.toHaveBeenCalled();
    expect(screen.queryByRole('dialog')).toBeNull();
  });

  it('gives Tab back to the browser: focus leaves the field, nothing toggles', async () => {
    const user = userEvent.setup();
    render(<App />);
    await settleApps();
    await settleHistory();

    const search = screen.getByRole('searchbox');
    expect(search).toHaveFocus();

    await user.keyboard('{Tab}');

    expect(search).not.toHaveFocus();
    // Nothing about the palette changed behind the focus move.
    expect(appsResults().getAllByRole('option').length).toBeGreaterThan(0);
    expect(historyResults().getAllByRole('option').length).toBeGreaterThan(0);
  });

  it('clears the query with Escape and never closes the palette', async () => {
    const user = userEvent.setup();
    render(<App />);
    await settleApps();
    await settleHistory();

    await user.type(screen.getByRole('searchbox'), 'notes');
    await user.keyboard('{Escape}');

    expect(screen.getByRole('searchbox')).toHaveValue('');
    expect(screen.getByRole('application', { name: 'Clipboard palette' })).toBeVisible();
    expect(appsResults().getAllByRole('option').length).toBeGreaterThan(0);
  });

  it('reports a failed launch without repeating the path it was given', async () => {
    const user = userEvent.setup();
    const launchApp = vi.fn(async () => {
      throw new Error('/private/wherever/the/app/was.app');
    });
    render(<App gateway={{ ...mockGateway, launchApp } as ClipboardGateway} />);

    await settleApps();
    await settleHistory();

    await user.type(screen.getByRole('searchbox'), 'terminal');
    await waitFor(() => expect(appsResults().getAllByRole('option')).toHaveLength(1));
    await user.keyboard('{Enter}');

    const alert = await screen.findByRole('alert');
    expect(alert).toHaveTextContent(/could not be started/i);
    expect(alert).not.toHaveTextContent('/private');
    expect(alert).not.toHaveTextContent('.app');
  });

  it('drops the launch alert when the user moves to a history row', async () => {
    const user = userEvent.setup();
    const launchApp = vi.fn(async () => {
      throw new Error('/private/wherever/the/app/was.app');
    });
    render(<App gateway={{ ...mockGateway, launchApp } as ClipboardGateway} />);

    await settleApps();
    await settleHistory();

    await user.type(screen.getByRole('searchbox'), 'terminal');
    await waitFor(() => expect(appsResults().getAllByRole('option')).toHaveLength(1));
    await user.keyboard('{Enter}');
    expect(await screen.findByRole('alert')).toBeVisible();

    // One list means one selection: a refusal that belongs to an application
    // row has no business staying on screen once the user is reading history.
    await user.keyboard('{Escape}');
    const historyRow = await waitFor(() => {
      const row = historyResults()
        .getAllByRole('option')
        .find((option) => option.hasAttribute('data-event-id'));
      expect(row).toBeDefined();
      return row as HTMLElement;
    });
    await user.click(historyRow);

    await waitFor(() => expect(screen.queryByRole('alert')).not.toBeInTheDocument());
  });

  it('leaves Tab to the dialog while one is open', async () => {
    const user = userEvent.setup();
    render(<App />);
    await settleApps();
    await settleHistory();

    await user.keyboard('{Meta>}i{/Meta}');
    expect(await screen.findByRole('dialog', { name: 'Import history' })).toBeVisible();

    await user.keyboard('{Tab}');

    expect(screen.getByRole('listbox', { name: 'Application and history results' })).toBeInTheDocument();
    expect(screen.getByRole('dialog', { name: 'Import history' })).toBeVisible();
  });
});
