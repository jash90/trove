import '@testing-library/jest-dom/vitest';
import { render, screen, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { useState, type KeyboardEventHandler } from 'react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import { useListNavigation } from '../hooks/useListNavigation';
import type { AppEntry } from '../lib/contracts';
import { AppsList } from './AppsList';

/// The application rows, and only those.
const appsList = () => within(screen.getByRole('listbox', { name: 'Application results' }));

const makeApps = (count: number, startAt = 0): AppEntry[] =>
  Array.from({ length: count }, (_, index) => {
    const serial = startAt + index;
    return {
      name: `Synthetic Application ${serial}`,
      bundleId: serial % 2 === 0 ? `com.example.synthetic${serial}` : null,
      path: `/synthetic/Applications/Synthetic Application ${serial}.app`,
    };
  });

interface KeyboardHarnessProps {
  apps: AppEntry[];
  onActivate: (path: string) => void;
}

const KeyboardHarness = ({
  apps,
  onActivate,
}: KeyboardHarnessProps): React.JSX.Element => {
  const [query, setQuery] = useState('');
  const navigation = useListNavigation({
    items: apps,
    keyOf: (app) => app.path,
    // The generic hook activates the whole item; the list's contract is a path.
    onActivate: (app) => onActivate(app.path),
    onEscape: () => setQuery(''),
  });

  const handleKeyDown: KeyboardEventHandler<HTMLInputElement> = (event) => {
    navigation.handleKeyDown(event);
  };

  return (
    <>
      <label htmlFor="apps-keyboard-search">Search applications</label>
      <input
        id="apps-keyboard-search"
        autoFocus
        value={query}
        aria-controls="apps-keyboard-results"
        aria-activedescendant={
          navigation.selectedKey === null
            ? undefined
            : `app-option-${apps.findIndex((app) => app.path === navigation.selectedKey)}`
        }
        onChange={(event) => setQuery(event.currentTarget.value)}
        onKeyDown={handleKeyDown}
      />
      <AppsList
        id="apps-keyboard-results"
        apps={apps}
        selectedKey={navigation.selectedKey}
        onSelect={navigation.setSelectedKey}
        onActivate={onActivate}
      />
    </>
  );
};

describe('AppsList', () => {
  beforeEach(() => {
    // jsdom reports zero-sized elements, so the virtualizer would render no rows.
    vi.spyOn(HTMLElement.prototype, 'offsetWidth', 'get').mockReturnValue(800);
    vi.spyOn(HTMLElement.prototype, 'offsetHeight', 'get').mockReturnValue(420);
  });

  afterEach(() => {
    vi.restoreAllMocks();
  });

  it('keeps the DOM bounded for a thousand applications', () => {
    render(
      <AppsList
        apps={makeApps(1_000)}
        selectedKey={makeApps(1)[0]!.path}
        onSelect={vi.fn()}
        onActivate={vi.fn()}
      />,
    );

    expect(appsList().getAllByRole('option').length).toBeLessThan(80);
  });

  it('exposes listbox options with stable path identity and no nested buttons', () => {
    const apps = makeApps(3);
    render(
      <AppsList
        apps={apps}
        selectedKey={apps[1]!.path}
        onSelect={vi.fn()}
        onActivate={vi.fn()}
      />,
    );

    const listbox = screen.getByRole('listbox', { name: 'Application results' });
    const selectedOption = appsList().getByRole('option', { selected: true });

    expect(listbox).toContainElement(selectedOption);
    expect(selectedOption).toHaveAttribute('data-path', apps[1]!.path);
    expect(selectedOption).toHaveAttribute('id', 'app-option-1');
    expect(listbox.querySelector('button')).toBeNull();
  });

  it('activates the selected application with Enter from the search field', async () => {
    const user = userEvent.setup();
    const onActivate = vi.fn();
    const apps = makeApps(3);
    render(<KeyboardHarness apps={apps} onActivate={onActivate} />);

    const input = screen.getByRole('textbox', { name: 'Search applications' });
    expect(input).toHaveFocus();

    await user.keyboard('{ArrowDown}{Enter}');

    expect(appsList().getByRole('option', { selected: true })).toHaveAttribute(
      'data-path',
      apps[1]!.path,
    );
    expect(onActivate).toHaveBeenCalledWith(apps[1]!.path);
    expect(input).toHaveFocus();
  });

  it('reconciles selection to the first application when the results change', () => {
    const onActivate = vi.fn();
    const { rerender } = render(<KeyboardHarness apps={makeApps(3)} onActivate={onActivate} />);

    rerender(<KeyboardHarness apps={makeApps(2, 20)} onActivate={onActivate} />);

    expect(appsList().getByRole('option', { selected: true })).toHaveAttribute(
      'data-path',
      '/synthetic/Applications/Synthetic Application 20.app',
    );
  });
});
