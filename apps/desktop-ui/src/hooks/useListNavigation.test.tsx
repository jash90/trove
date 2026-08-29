import '@testing-library/jest-dom/vitest';
import { render, screen, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { useState, type KeyboardEventHandler } from 'react';
import { describe, expect, it, vi } from 'vitest';

import { useListNavigation } from './useListNavigation';

interface Row {
  key: string;
  label: string;
}

const rows: Row[] = [
  { key: 'app-one', label: 'One' },
  { key: 'app-two', label: 'Two' },
  { key: 'app-three', label: 'Three' },
];

interface HarnessProps {
  items: Row[];
  onActivate: (row: Row) => void;
}

const Harness = ({ items, onActivate }: HarnessProps): React.JSX.Element => {
  const [query, setQuery] = useState('');
  const navigation = useListNavigation({
    items,
    keyOf: (row) => row.key,
    onActivate,
    onEscape: () => setQuery(''),
  });
  const handleKeyDown: KeyboardEventHandler<HTMLInputElement> = (event) => {
    navigation.handleKeyDown(event);
  };
  return (
    <>
      <label htmlFor="navigation-search">Search</label>
      <input
        id="navigation-search"
        autoFocus
        value={query}
        onChange={(event) => setQuery(event.currentTarget.value)}
        onKeyDown={handleKeyDown}
      />
      {/* The listbox/option pairing is what carries `aria-selected`; the
          generic hook does not care, but the real lists do, and so does the
          query this test asserts with. */}
      <div role="listbox" aria-label="results">
        {items.map((row) => (
          <div
            key={row.key}
            role="option"
            aria-selected={navigation.selectedKey === row.key}
            data-key={row.key}
          >
            {row.label}
          </div>
        ))}
      </div>
    </>
  );
};

const resultListbox = () => within(screen.getByRole('listbox', { name: 'results' }));

describe('useListNavigation', () => {
  it('moves with arrows and Home and End over string keys', async () => {
    const user = userEvent.setup();
    render(<Harness items={rows} onActivate={vi.fn()} />);
    const input = screen.getByRole('textbox', { name: 'Search' });

    await user.keyboard('{ArrowDown}');
    expect(resultListbox().getByRole('option', { selected: true })).toHaveAttribute(
      'data-key',
      'app-two',
    );

    await user.keyboard('{End}');
    expect(resultListbox().getByRole('option', { selected: true })).toHaveAttribute(
      'data-key',
      'app-three',
    );

    await user.keyboard('{Home}');
    expect(resultListbox().getByRole('option', { selected: true })).toHaveAttribute(
      'data-key',
      'app-one',
    );
    expect(input).toHaveFocus();
  });

  it('activates the selected item with Enter and clears the query with Escape', async () => {
    const user = userEvent.setup();
    const onActivate = vi.fn();
    render(<Harness items={rows} onActivate={onActivate} />);
    const input = screen.getByRole('textbox', { name: 'Search' });

    await user.type(input, 'anything');
    await user.keyboard('{ArrowDown}{Enter}');
    expect(onActivate).toHaveBeenCalledWith(rows[1]);

    await user.keyboard('{Escape}');
    expect(input).toHaveValue('');
  });

  it('reconciles the selection to the first item when the list replaces it', () => {
    const { rerender } = render(<Harness items={rows} onActivate={vi.fn()} />);

    rerender(
      <Harness
        items={[
          { key: 'app-four', label: 'Four' },
          { key: 'app-five', label: 'Five' },
        ]}
        onActivate={vi.fn()}
      />,
    );

    expect(resultListbox().getByRole('option', { selected: true })).toHaveAttribute(
      'data-key',
      'app-four',
    );
  });

  it('does not activate when the list is empty', async () => {
    const user = userEvent.setup();
    const onActivate = vi.fn();
    render(<Harness items={[]} onActivate={onActivate} />);

    await user.keyboard('{Enter}');

    expect(onActivate).not.toHaveBeenCalled();
  });
});
