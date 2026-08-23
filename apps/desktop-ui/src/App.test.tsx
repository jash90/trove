import '@testing-library/jest-dom/vitest';
import { render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { App } from './App';

it('renders the private clipboard palette landmark', () => {
  render(<App />);
  expect(screen.getByRole('application', { name: 'Historia schowka' })).toBeVisible();
});

it('opens one real header workspace at a time and restores its invoker focus', async () => {
  const user = userEvent.setup();
  render(<App />);
  const importButton = screen.getByRole('button', { name: 'Importuj archiwum' });
  const settingsButton = screen.getByRole('button', { name: 'Otwórz ustawienia' });

  await user.click(importButton);
  expect(screen.getByRole('dialog', { name: 'Importuj historię' })).toBeVisible();
  expect(screen.queryByRole('dialog', { name: 'Ustawienia' })).not.toBeInTheDocument();
  expect(screen.getByLabelText('Paleta historii schowka')).toHaveAttribute('inert');

  await user.click(screen.getByRole('button', { name: 'Zamknij import' }));
  await waitFor(() => expect(importButton).toHaveFocus());
  expect(screen.getByLabelText('Paleta historii schowka')).not.toHaveAttribute('inert');

  await user.click(settingsButton);
  expect(await screen.findByRole('dialog', { name: 'Ustawienia' })).toBeVisible();
  expect(screen.queryByRole('dialog', { name: 'Importuj historię' })).not.toBeInTheDocument();

  await user.click(screen.getByRole('button', { name: 'Zamknij ustawienia' }));
  await waitFor(() => expect(settingsButton).toHaveFocus());
});
