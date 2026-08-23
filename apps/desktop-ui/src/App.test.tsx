import '@testing-library/jest-dom/vitest';
import { render, screen } from '@testing-library/react';
import { App } from './App';

it('renders the private clipboard palette landmark', () => {
  render(<App />);
  expect(screen.getByRole('application', { name: 'Historia schowka' })).toBeVisible();
});
