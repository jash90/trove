import '@testing-library/jest-dom/vitest';
import { render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { App } from './App';

it('renders the private clipboard palette landmark', () => {
  render(<App />);
  expect(screen.getByRole('application', { name: 'Historia schowka' })).toBeVisible();
});

it('opens one workspace at a time and returns focus to the search field', async () => {
  const user = userEvent.setup();
  render(<App />);
  const search = screen.getByRole('searchbox', { name: 'Przeszukaj historię' });

  await user.click(screen.getByRole('button', { name: 'Importuj archiwum' }));
  expect(screen.getByRole('dialog', { name: 'Importuj historię' })).toBeVisible();
  expect(screen.queryByRole('dialog', { name: 'Ustawienia' })).not.toBeInTheDocument();
  expect(screen.getByLabelText('Paleta historii schowka')).toHaveAttribute('inert');

  await user.click(screen.getByRole('button', { name: 'Zamknij import' }));
  // The palette has one place a keyboard user works from, and a shortcut has
  // no button to hand focus back to.
  await waitFor(() => expect(search).toHaveFocus());
  expect(screen.getByLabelText('Paleta historii schowka')).not.toHaveAttribute('inert');

  await user.click(screen.getByRole('button', { name: 'Otwórz ustawienia' }));
  expect(await screen.findByRole('dialog', { name: 'Ustawienia' })).toBeVisible();
  expect(screen.queryByRole('dialog', { name: 'Importuj historię' })).not.toBeInTheDocument();

  await user.click(screen.getByRole('button', { name: 'Zamknij ustawienia' }));
  await waitFor(() => expect(search).toHaveFocus());
});

it('opens both workspaces from the keyboard, with no history selected', async () => {
  const user = userEvent.setup();
  render(<App />);

  await user.keyboard('{Meta>}i{/Meta}');
  expect(await screen.findByRole('dialog', { name: 'Importuj historię' })).toBeVisible();
  await user.click(screen.getByRole('button', { name: 'Zamknij import' }));

  await user.keyboard('{Meta>},{/Meta}');
  expect(await screen.findByRole('dialog', { name: 'Ustawienia' })).toBeVisible();
});
