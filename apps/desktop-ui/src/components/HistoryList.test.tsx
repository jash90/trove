import '@testing-library/jest-dom/vitest';
import { act, render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { useState, type KeyboardEventHandler } from 'react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import { App } from '../App';
import { useKeyboardNavigation } from '../hooks/useKeyboardNavigation';
import type { HistoryItem, HistoryPage } from '../lib/contracts';
import type { ClipboardGateway } from '../lib/gateway';
import { FilterRail } from './FilterRail';
import { HistoryList } from './HistoryList';

const makeItems = (count: number, startAt = 1): HistoryItem[] =>
  Array.from({ length: count }, (_, index) => {
    const eventId = startAt + index;
    return {
      eventId,
      globalId: `0198f000-0000-7000-8000-${String(eventId).padStart(12, '0')}`,
      kind: eventId % 2 === 0 ? 'link' : 'text',
      capturedAtMs: 1_775_000_000_000 - index * 1_000,
      sourceAppName: 'Synthetic Editor',
      pinned: eventId === 1,
      preview: `Synthetic clipboard item ${eventId}`,
      byteSize: 32,
      hasThumbnail: false,
    };
  });

interface KeyboardHarnessProps {
  items: HistoryItem[];
  onActivate: (eventId: number) => void;
}

const KeyboardHarness = ({ items, onActivate }: KeyboardHarnessProps): React.JSX.Element => {
  const [query, setQuery] = useState('private phrase');
  const navigation = useKeyboardNavigation({
    items,
    onActivate,
    onEscape: () => setQuery(''),
  });

  const handleKeyDown: KeyboardEventHandler<HTMLInputElement> = (event) => {
    navigation.handleKeyDown(event);
  };

  return (
    <>
      <label htmlFor="keyboard-search">Przeszukaj historię</label>
      <input
        id="keyboard-search"
        autoFocus
        value={query}
        aria-controls="keyboard-results"
        aria-activedescendant={
          navigation.selectedId === null
            ? undefined
            : `history-option-${navigation.selectedId}`
        }
        onChange={(event) => setQuery(event.currentTarget.value)}
        onKeyDown={handleKeyDown}
      />
      <HistoryList
        id="keyboard-results"
        items={items}
        selectedId={navigation.selectedId}
        onSelect={navigation.setSelectedId}
        onActivate={onActivate}
      />
    </>
  );
};

const makeGateway = (search: ClipboardGateway['search']): ClipboardGateway =>
  ({
    search,
    preview: vi.fn(async (eventId: number) => ({
      eventId,
      kind: 'text',
      mimeType: 'text/plain',
      text: `Synthetic clipboard item ${eventId}`,
      byteSize: 32,
      sourceAppName: 'Synthetic Editor',
    })),
    getThumbnail: vi.fn(async () => null),
    copyEvent: vi.fn(async () => ({ mode: 'copied', plainText: false })),
  }) as unknown as ClipboardGateway;

describe('HistoryList', () => {
  beforeEach(() => {
    vi.spyOn(HTMLElement.prototype, 'offsetWidth', 'get').mockReturnValue(800);
    vi.spyOn(HTMLElement.prototype, 'offsetHeight', 'get').mockReturnValue(420);
  });

  afterEach(() => {
    vi.restoreAllMocks();
  });

  it('keeps the DOM bounded for ten thousand clipboard events', () => {
    render(
      <HistoryList
        items={makeItems(10_000)}
        selectedId={1}
        onSelect={vi.fn()}
        onActivate={vi.fn()}
      />,
    );

    expect(screen.getAllByRole('option').length).toBeLessThan(80);
  });

  it('exposes listbox options with stable event identity and no nested buttons', () => {
    render(
      <HistoryList
        items={makeItems(3)}
        selectedId={2}
        onSelect={vi.fn()}
        onActivate={vi.fn()}
      />,
    );

    const listbox = screen.getByRole('listbox', { name: 'Wyniki historii schowka' });
    const selectedOption = screen.getByRole('option', { selected: true });

    expect(listbox).toContainElement(selectedOption);
    expect(selectedOption).toHaveAttribute('data-event-id', '2');
    expect(selectedOption).toHaveAttribute('id', 'history-option-2');
    expect(listbox.querySelector('button')).toBeNull();
  });

  it('moves selection with ArrowDown and activates the selected event with Enter', async () => {
    const user = userEvent.setup();
    const onActivate = vi.fn();
    render(<KeyboardHarness items={makeItems(3)} onActivate={onActivate} />);

    const input = screen.getByRole('textbox', { name: 'Przeszukaj historię' });
    expect(input).toHaveFocus();

    await user.keyboard('{ArrowDown}{Enter}');

    expect(screen.getByRole('option', { selected: true })).toHaveAttribute(
      'data-event-id',
      '2',
    );
    expect(onActivate).toHaveBeenCalledWith(2);
    expect(input).toHaveFocus();
  });

  it('supports Home, End, ArrowUp, and Escape without moving focus from search', async () => {
    const user = userEvent.setup();
    render(<KeyboardHarness items={makeItems(4)} onActivate={vi.fn()} />);

    const input = screen.getByRole('textbox', { name: 'Przeszukaj historię' });
    await user.keyboard('{End}{ArrowUp}');
    expect(screen.getByRole('option', { selected: true })).toHaveAttribute(
      'data-event-id',
      '3',
    );

    await user.keyboard('{Home}{Escape}');
    expect(screen.getByRole('option', { selected: true })).toHaveAttribute(
      'data-event-id',
      '1',
    );
    expect(input).toHaveValue('');
    expect(input).toHaveFocus();
  });

  it('reconciles selection to the first available event when results change', () => {
    const onActivate = vi.fn();
    const { rerender } = render(
      <KeyboardHarness items={makeItems(3)} onActivate={onActivate} />,
    );

    rerender(<KeyboardHarness items={makeItems(2, 20)} onActivate={onActivate} />);

    expect(screen.getByRole('option', { selected: true })).toHaveAttribute(
      'data-event-id',
      '20',
    );
  });

  it('commits reconciled selection so a removed event cannot reactivate when reintroduced', async () => {
    const user = userEvent.setup();
    const onActivate = vi.fn();
    const { rerender } = render(
      <KeyboardHarness items={makeItems(2)} onActivate={onActivate} />,
    );

    await user.keyboard('{ArrowDown}');
    expect(screen.getByRole('option', { selected: true })).toHaveAttribute(
      'data-event-id',
      '2',
    );

    rerender(<KeyboardHarness items={makeItems(2, 3)} onActivate={onActivate} />);
    await waitFor(() => {
      expect(screen.getByRole('option', { selected: true })).toHaveAttribute(
        'data-event-id',
        '3',
      );
    });

    rerender(
      <KeyboardHarness
        items={[makeItems(1, 2)[0]!, ...makeItems(2, 3)]}
        onActivate={onActivate}
      />,
    );
    expect(screen.getByRole('option', { selected: true })).toHaveAttribute(
      'data-event-id',
      '3',
    );
  });
});

describe('FilterRail', () => {
  it.each([
    ['Tekst', 'roadmap app:Editor type:text'],
    ['Linki', 'roadmap app:Editor type:link'],
    ['Obrazy', 'roadmap app:Editor type:image'],
    ['Pliki', 'roadmap app:Editor type:file'],
    ['Przypięte', 'roadmap app:Editor is:pinned'],
    ['Wszystkie', 'roadmap app:Editor'],
  ])('emits the supported query for the %s filter', async (label, expectedQuery) => {
    const user = userEvent.setup();
    const handleQueryChange = vi.fn();
    render(
      <FilterRail
        query="roadmap type:image app:Editor is:pinned"
        onQueryChange={handleQueryChange}
      />,
    );

    await user.click(screen.getByRole('button', { name: label }));

    expect(handleQueryChange).toHaveBeenCalledWith(expectedQuery);
  });

  it('clears a manually entered supported type when returning to all items', async () => {
    const user = userEvent.setup();
    const handleQueryChange = vi.fn();
    render(
      <FilterRail query="type:color app:Editor" onQueryChange={handleQueryChange} />,
    );

    await user.click(screen.getByRole('button', { name: 'Wszystkie' }));

    expect(handleQueryChange).toHaveBeenCalledWith('app:Editor');
  });
});

describe('clipboard palette states', () => {
  afterEach(() => {
    vi.useRealTimers();
    vi.restoreAllMocks();
  });

  it('focuses the query first and sends a filter token through the gateway', async () => {
    vi.useFakeTimers();
    const page: HistoryPage = {
      items: makeItems(2),
      nextCursor: null,
      rankedTruncated: false,
    };
    const search = vi.fn<ClipboardGateway['search']>(async () => page);
    render(<App gateway={makeGateway(search)} />);

    expect(screen.getByRole('searchbox', { name: 'Przeszukaj historię' })).toHaveFocus();

    await act(async () => {
      await vi.advanceTimersByTimeAsync(150);
    });
    await act(async () => {
      screen.getByRole('button', { name: 'Obrazy' }).click();
      await vi.advanceTimersByTimeAsync(150);
    });

    expect(search).toHaveBeenLastCalledWith({
      query: 'type:image',
      limit: 80,
      cursor: null,
    });
  });

  it('returns focus to search after pointer-selecting a history row', async () => {
    const user = userEvent.setup();
    vi.spyOn(HTMLElement.prototype, 'offsetWidth', 'get').mockReturnValue(800);
    vi.spyOn(HTMLElement.prototype, 'offsetHeight', 'get').mockReturnValue(420);
    const page: HistoryPage = {
      items: makeItems(2),
      nextCursor: null,
      rankedTruncated: false,
    };
    render(<App gateway={makeGateway(async () => page)} />);
    const selectedRow = await screen.findByRole('option', {
      name: /Synthetic clipboard item 2/,
    });
    const search = screen.getByRole('searchbox', { name: 'Przeszukaj historię' });

    await user.click(selectedRow);

    expect(search).toHaveFocus();
    expect(screen.getByRole('option', { selected: true })).toHaveAttribute(
      'data-event-id',
      '2',
    );
  });

  it('returns focus to search after activating a filter with a pointer', async () => {
    const user = userEvent.setup();
    const page: HistoryPage = {
      items: makeItems(2),
      nextCursor: null,
      rankedTruncated: false,
    };
    render(<App gateway={makeGateway(async () => page)} />);
    await screen.findByRole('listbox', { name: 'Wyniki historii schowka' });
    const search = screen.getByRole('searchbox', { name: 'Przeszukaj historię' });

    await user.click(screen.getByRole('button', { name: 'Obrazy' }));

    expect(search).toHaveFocus();
  });

  it('announces loading, empty, and generic error states without leaking details', async () => {
    vi.useFakeTimers();
    const pending = new Promise<HistoryPage>(() => undefined);
    const { rerender } = render(<App gateway={makeGateway(() => pending)} />);

    expect(screen.getByRole('status')).toHaveTextContent('Ładowanie historii');

    const emptyGateway = makeGateway(async () => ({
      items: [],
      nextCursor: null,
      rankedTruncated: false,
    }));
    rerender(<App gateway={emptyGateway} />);
    await act(async () => {
      await vi.advanceTimersByTimeAsync(150);
    });
    expect(screen.getByRole('status')).toHaveTextContent('Historia jest pusta');

    const errorGateway = makeGateway(async () => {
      throw new Error('/private/archive.json contains private query text');
    });
    rerender(<App gateway={errorGateway} />);
    await act(async () => {
      await vi.advanceTimersByTimeAsync(150);
    });

    const alert = screen.getByRole('alert');
    expect(alert).toHaveTextContent('Nie udało się wczytać historii');
    expect(alert).not.toHaveTextContent('private');
    expect(alert).not.toHaveTextContent('archive.json');
  });
});
