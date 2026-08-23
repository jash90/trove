import '@testing-library/jest-dom/vitest';
import { render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { expect, it, vi } from 'vitest';
import { App } from './App';
import { mockGateway } from './lib/gateway';

it('renders the private clipboard palette landmark', () => {
  render(<App />);
  expect(screen.getByRole('application', { name: 'Historia schowka' })).toBeVisible();
});

it('opens the import wizard over the palette and returns focus to the search field', async () => {
  const user = userEvent.setup();
  render(<App />);
  const search = screen.getByRole('searchbox', { name: 'Przeszukaj historię' });

  await user.click(screen.getByRole('button', { name: 'Importuj archiwum' }));
  expect(screen.getByRole('dialog', { name: 'Importuj historię' })).toBeVisible();
  expect(screen.getByLabelText('Paleta historii schowka')).toHaveAttribute('inert');

  await user.click(screen.getByRole('button', { name: 'Zamknij import' }));
  // The palette has one place a keyboard user works from, and a shortcut has
  // no button to hand focus back to.
  await waitFor(() => expect(search).toHaveFocus());
  expect(screen.getByLabelText('Paleta historii schowka')).not.toHaveAttribute('inert');
});

it('asks for the settings window rather than covering the list with a dialog', async () => {
  const user = userEvent.setup();
  const openSettingsWindow = vi.fn(async () => undefined);
  render(<App gateway={{ ...mockGateway, openSettingsWindow }} />);

  await user.click(screen.getByRole('button', { name: 'Otwórz ustawienia' }));
  expect(openSettingsWindow).toHaveBeenCalledOnce();

  await user.keyboard('{Meta>},{/Meta}');
  expect(openSettingsWindow).toHaveBeenCalledTimes(2);
  // Nothing was drawn over the palette either way.
  expect(screen.queryByRole('dialog', { name: 'Ustawienia' })).not.toBeInTheDocument();
});

it('opens the import wizard from the keyboard, with no history selected', async () => {
  const user = userEvent.setup();
  render(<App />);

  await user.keyboard('{Meta>}i{/Meta}');
  expect(await screen.findByRole('dialog', { name: 'Importuj historię' })).toBeVisible();
});
