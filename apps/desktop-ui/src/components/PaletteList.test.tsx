import '@testing-library/jest-dom/vitest';
import { act, render, screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { useState, type KeyboardEventHandler } from 'react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import { App } from '../App';
import { useListNavigation } from '../hooks/useListNavigation';
import type { AppEntry, HistoryItem, HistoryPage } from '../lib/contracts';
import { SYNTHETIC_APP_ICON } from '../lib/fixtures';
import { GatewayProvider, type ClipboardGateway } from '../lib/gateway';
import { buildPaletteItems, keyOfItem, type PaletteItem } from '../lib/paletteItems';
import { PaletteList } from './PaletteList';
import { TypeFilter } from './TypeFilter';

/// The palette's rows, and only those.
///
/// The type filter beside the search field is a combobox, and its choices are
/// options too. An unscoped option query matches both, so a test can pass
/// while the list it meant to inspect has not loaded at all.
const paletteList = () =>
  within(screen.getByRole('listbox', { name: 'Applications, secrets and history results' }));

const makeApps = (count: number, startAt = 1): AppEntry[] =>
  Array.from({ length: count }, (_, index) => ({
    name: `Synthetic App ${startAt + index}`,
    bundleId: `app.synthetic.${startAt + index}`,
    path: `/Applications/Synthetic App ${startAt + index}.app`,
  }));

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
      occurrenceCount: 1,
      occurrences: [1_775_000_000_000 - index * 1_000],
    };
  });

interface KeyboardHarnessProps {
  items: PaletteItem[];
  onActivate: (entry: PaletteItem) => void;
}

const KeyboardHarness = ({ items, onActivate }: KeyboardHarnessProps): React.JSX.Element => {
  const [query, setQuery] = useState('private phrase');
  const navigation = useListNavigation({
    items,
    keyOf: keyOfItem,
    onActivate,
    onEscape: () => setQuery(''),
  });

  const handleKeyDown: KeyboardEventHandler<HTMLInputElement> = (event) => {
    navigation.handleKeyDown(event);
  };

  // The harness announces the same relation the palette does: the listbox
  // PaletteList renders carries id="history-results", and the active option
  // is the row's own id — `app-option-${index}` or `history-option-${eventId}`
  // — not a key invented here. A harness that points at neither would let a
  // broken ARIA wiring pass its own test.
  const selectedIndex = items.findIndex((entry) => keyOfItem(entry) === navigation.selectedKey);
  const selectedEntry = selectedIndex < 0 ? undefined : items[selectedIndex];
  const activeDescendant =
    selectedEntry === undefined
      ? undefined
      : selectedEntry.kind === 'app'
        ? `app-option-${selectedIndex}`
        : selectedEntry.kind === 'vault'
          ? `vault-option-${selectedIndex}`
          : `history-option-${selectedEntry.item.eventId}`;

  return (
    <>
      <label htmlFor="keyboard-search">Search applications, secrets and history</label>
      <input
        id="keyboard-search"
        autoFocus
        value={query}
        aria-controls="history-results"
        aria-activedescendant={activeDescendant}
        onChange={(event) => setQuery(event.currentTarget.value)}
        onKeyDown={handleKeyDown}
      />
      <PaletteList
        items={items}
        selectedKey={navigation.selectedKey}
        onSelect={(entry) => navigation.setSelectedKey(keyOfItem(entry))}
        onActivate={onActivate}
      />
    </>
  );
};

const makeGateway = (search: ClipboardGateway['search']): ClipboardGateway =>
  ({
    search,
    listApps: vi.fn(async () => []),
    // The palette asks the vault once a query is typed; without this the hook throws where a
    // rejected promise would have been handled, and takes the render down with it.
    keyvaultList: vi.fn(async () => []),
    keyvaultCopySecret: vi.fn(async () => undefined),
    preview: vi.fn(async (eventId: number) => ({
      eventId,
      kind: 'text',
      mimeType: 'text/plain',
      text: `Synthetic clipboard item ${eventId}`,
      byteSize: 32,
      sourceAppName: 'Synthetic Editor',
    })),
    getThumbnail: vi.fn(async () => null),
    linkPreview: vi.fn(async () => null),
    copyEvent: vi.fn(async () => ({ mode: 'copied' as const, plainText: false })),
    getAppIcon: vi.fn(async () => null),
  }) as unknown as ClipboardGateway;

const bareGateway: ClipboardGateway = {
  getAppIcon: async () => null,
} as unknown as ClipboardGateway;

const renderList = (ui: React.JSX.Element): ReturnType<typeof render> =>
  render(<GatewayProvider gateway={bareGateway}>{ui}</GatewayProvider>);

describe('PaletteList', () => {
  beforeEach(() => {
    vi.spyOn(HTMLElement.prototype, 'offsetWidth', 'get').mockReturnValue(800);
    vi.spyOn(HTMLElement.prototype, 'offsetHeight', 'get').mockReturnValue(420);
  });

  afterEach(() => {
    vi.restoreAllMocks();
  });

  it('keeps the DOM bounded for ten thousand mixed rows', () => {
    renderList(
      <PaletteList
        items={buildPaletteItems(makeApps(5_000), makeItems(5_000), 'match')}
        selectedKey={null}
        onSelect={vi.fn()}
        onActivate={vi.fn()}
      />,
    );

    expect(paletteList().getAllByRole('option').length).toBeLessThan(80);
  });

  it('exposes options with stable identity and no nested buttons', () => {
    const items = buildPaletteItems(makeApps(2), makeItems(2), 'match');
    renderList(
      <PaletteList
        items={items}
        selectedKey={items[1]?.kind === 'app' ? items[1].app.path : null}
        onSelect={vi.fn()}
        onActivate={vi.fn()}
      />,
    );

    const listbox = screen.getByRole('listbox', { name: 'Applications, secrets and history results' });
    expect(listbox.querySelector('button')).toBeNull();
    const appRow = paletteList().getByRole('option', { name: /Synthetic App 1/ });
    expect(appRow).toHaveAttribute('data-path', items[0]?.kind === 'app' ? items[0].app.path : '');
    expect(appRow).toHaveAttribute('id', 'app-option-0');
    const historyRow = paletteList().getByRole('option', { name: /Synthetic clipboard item 1/ });
    expect(historyRow).toHaveAttribute('data-event-id', '1');
    expect(historyRow).toHaveAttribute('id', 'history-option-1');
  });

  it('offers a vault secret by name and copies it through the core on activation', async () => {
    const user = userEvent.setup();
    const activated: string[] = [];
    const items = buildPaletteItems([], [], 'stripe', [
      { slug: 'stripe-secret-key', name: 'Stripe secret key', category: 'payments' },
    ]);
    renderList(
      <KeyboardHarness
        items={items}
        onActivate={(entry) => {
          activated.push(
            entry.kind === 'app'
              ? `app:${entry.app.path}`
              : entry.kind === 'vault'
                ? `vault:${entry.secret.slug}`
                : `history:${entry.item.eventId}`,
          );
        }}
      />,
    );

    const row = paletteList().getByRole('option', { name: /Vault secret: Stripe secret key/ });
    expect(row).toHaveAttribute('data-slug', 'stripe-secret-key');
    // The slug and category are on the row; the value is not, and never passes through here.
    expect(row).toHaveTextContent('payments');
    expect(row.textContent).not.toContain('sk_');

    await user.keyboard('{Home}{Enter}');
    expect(activated).toEqual(['vault:stripe-secret-key']);
  });

  it('moves selection with ArrowDown across the app/history boundary and activates the right kind', async () => {
    const user = userEvent.setup();
    const activated: string[] = [];
    const items = buildPaletteItems(makeApps(2), makeItems(2), 'match');
    renderList(
      <KeyboardHarness
        items={items}
        onActivate={(entry) => {
          activated.push(
            entry.kind === 'app'
              ? `app:${entry.app.path}`
              : entry.kind === 'vault'
                ? `vault:${entry.secret.slug}`
                : `history:${entry.item.eventId}`,
          );
        }}
      />,
    );

    const input = screen.getByRole('textbox', { name: 'Search applications, secrets and history' });
    expect(input).toHaveFocus();
    // Home to the first row, then Down into the history half.
    await user.keyboard('{Home}{ArrowDown}');

    const selected = paletteList().getByRole('option', { selected: true });
    expect(selected).toHaveAttribute('data-event-id', '1');
    await user.keyboard('{Enter}');
    expect(activated).toEqual(['history:1']);

    // Home again, Enter on the first application: activation crosses back.
    await user.keyboard('{Home}{Enter}');
    expect(activated).toEqual(['history:1', 'app:/Applications/Synthetic App 1.app']);
    expect(input).toHaveFocus();
  });

  it('supports Home, End, ArrowUp, and Escape without moving focus from search', async () => {
    const user = userEvent.setup();
    renderList(
      <KeyboardHarness
        items={buildPaletteItems(makeApps(2), makeItems(2), 'match')}
        onActivate={vi.fn()}
      />,
    );

    const input = screen.getByRole('textbox', { name: 'Search applications, secrets and history' });
    await user.keyboard('{End}{ArrowUp}');
    // End lands on the last history row; ArrowUp crosses back onto the
    // last application of the apps half.
    expect(paletteList().getByRole('option', { selected: true })).toHaveAttribute(
      'data-path',
      '/Applications/Synthetic App 2.app',
    );

    await user.keyboard('{Home}{Escape}');
    expect(paletteList().getByRole('option', { selected: true })).toHaveAttribute(
      'data-path',
      '/Applications/Synthetic App 1.app',
    );
    expect(input).toHaveValue('');
    expect(input).toHaveFocus();
  });

  it('reconciles selection to the first row when results change', () => {
    const onActivate = vi.fn();
    const { rerender } = renderList(
      <KeyboardHarness items={buildPaletteItems(makeApps(3), makeItems(3), 'match')} onActivate={onActivate} />,
    );

    rerender(
      <GatewayProvider gateway={bareGateway}>
        <KeyboardHarness items={buildPaletteItems(makeApps(2, 20), makeItems(2, 30), 'match')} onActivate={onActivate} />
      </GatewayProvider>,
    );

    expect(paletteList().getByRole('option', { selected: true })).toHaveAttribute(
      'data-path',
      '/Applications/Synthetic App 20.app',
    );
  });
});

describe('TypeFilter', () => {
  it.each([
    ['Text', 'roadmap app:Editor type:text'],
    ['Links', 'roadmap app:Editor type:link'],
    ['Images', 'roadmap app:Editor type:image'],
    ['Files', 'roadmap app:Editor type:file'],
    ['Pinned', 'roadmap app:Editor is:pinned'],
    ['Wszystkie', 'roadmap app:Editor'],
  ])('emits the supported query for the %s filter', async (label, expectedQuery) => {
    const user = userEvent.setup();
    const handleQueryChange = vi.fn();
    render(
      <TypeFilter
        query="roadmap type:image app:Editor is:pinned"
        onQueryChange={handleQueryChange}
      />,
    );

    await user.selectOptions(screen.getByRole('combobox', { name: 'Filtr typu' }), label);

    expect(handleQueryChange).toHaveBeenCalledWith(expectedQuery);
  });

  it('clears a manually entered supported type when returning to all items', async () => {
    const user = userEvent.setup();
    const handleQueryChange = vi.fn();
    render(<TypeFilter query="type:color app:Editor" onQueryChange={handleQueryChange} />);

    await user.selectOptions(screen.getByRole('combobox', { name: 'Filtr typu' }), 'Wszystkie');

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

    expect(screen.getByRole('searchbox', { name: 'Search applications, secrets and history' })).toHaveFocus();

    await act(async () => {
      await vi.advanceTimersByTimeAsync(150);
    });
    await act(async () => {
      const filter = screen.getByRole('combobox', { name: 'Filtr typu' }) as HTMLSelectElement;
      filter.value = 'image';
      filter.dispatchEvent(new Event('change', { bubbles: true }));
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
    await screen.findByRole('listbox', { name: 'Applications, secrets and history results' });
    const selectedRow = paletteList().getByRole('option', {
      name: /Synthetic clipboard item 2/,
    });
    const search = screen.getByRole('searchbox', { name: 'Search applications, secrets and history' });

    await user.click(selectedRow);

    expect(search).toHaveFocus();
    expect(paletteList().getByRole('option', { selected: true })).toHaveAttribute(
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
    await screen.findByRole('listbox', { name: 'Applications, secrets and history results' });
    const search = screen.getByRole('searchbox', { name: 'Search applications, secrets and history' });

    await user.selectOptions(screen.getByRole('combobox', { name: 'Filtr typu' }), 'Images');

    expect(search).toHaveFocus();
  });

  it('announces loading, empty, and generic error states without leaking details', async () => {
    vi.useFakeTimers();
    const pending = new Promise<HistoryPage>(() => undefined);
    const { rerender } = render(<App gateway={makeGateway(() => pending)} />);

    expect(screen.getByRole('status')).toHaveTextContent('Loading history');

    const emptyGateway = makeGateway(async () => ({
      items: [],
      nextCursor: null,
      rankedTruncated: false,
    }));
    rerender(<App gateway={emptyGateway} />);
    await act(async () => {
      await vi.advanceTimersByTimeAsync(150);
    });
    expect(screen.getByRole('status')).toHaveTextContent('The history is empty');

    const errorGateway = makeGateway(async () => {
      throw new Error('/private/archive.json contains private query text');
    });
    rerender(<App gateway={errorGateway} />);
    await act(async () => {
      await vi.advanceTimersByTimeAsync(150);
    });

    const alert = screen.getByRole('alert');
    expect(alert).toHaveTextContent('The history could not be loaded');
    expect(alert).not.toHaveTextContent('private');
    expect(alert).not.toHaveTextContent('archive.json');
  });

  it('speaks for the catalog when the history answered and had nothing', async () => {
    vi.useFakeTimers();
    const empty = async (): Promise<HistoryPage> => ({
      items: [],
      nextCursor: null,
      rankedTruncated: false,
    });

    // History ready and empty, catalog still reading the bundles: without a
    // state of its own the panel would say nothing at all.
    const loadingCatalog = {
      ...makeGateway(empty),
      listApps: vi.fn(() => new Promise<AppEntry[]>(() => undefined)),
    } as unknown as ClipboardGateway;
    const { rerender } = render(<App gateway={loadingCatalog} />);
    await act(async () => {
      await vi.advanceTimersByTimeAsync(150);
    });
    expect(screen.getByRole('status')).toHaveTextContent('Loading applications');

    // And a catalog that failed must say so rather than fail silently.
    const brokenCatalog = {
      ...makeGateway(empty),
      listApps: vi.fn(async () => {
        throw new Error('/private/Applications is not a place to name');
      }),
    } as unknown as ClipboardGateway;
    rerender(<App gateway={brokenCatalog} />);
    await act(async () => {
      await vi.advanceTimersByTimeAsync(150);
    });

    const catalogAlert = screen.getByRole('alert');
    expect(catalogAlert).toHaveTextContent('The applications could not be loaded');
    expect(catalogAlert).not.toHaveTextContent('private');
  });
});

describe('grouped rows', () => {
  beforeEach(() => {
    vi.spyOn(HTMLElement.prototype, 'offsetWidth', 'get').mockReturnValue(800);
    vi.spyOn(HTMLElement.prototype, 'offsetHeight', 'get').mockReturnValue(420);
  });

  afterEach(() => {
    vi.restoreAllMocks();
  });

  it('shows how many times a repeated capture was recorded, and hides the badge for one', () => {
    const single = makeItems(2);
    const grouped: HistoryItem = {
      ...makeItems(1, 9)[0]!,
      occurrenceCount: 3,
      occurrences: [1_775_000_000_000, 1_774_000_000_000, 1_773_000_000_000],
    };
    renderList(
      <PaletteList
        items={buildPaletteItems([], [grouped, ...single], '')}
        selectedKey={null}
        onSelect={() => undefined}
        onActivate={() => undefined}
      />,
    );

    const row = paletteList().getByRole('option', {
      name: /Synthetic clipboard item 9/,
    });
    expect(within(row).getByText('×3')).toBeVisible();
    expect(within(row).getByLabelText('Captured 3 times')).toBeVisible();
    // Children of an option are presentational, so the count must also ride
    // the row's own accessible name — that is what a screen reader reads.
    expect(
      paletteList().getByRole('option', {
        name: /Synthetic clipboard item 9, captured 3 times/u,
      }),
    ).toBe(row);

    const once = paletteList().getByRole('option', {
      name: /Synthetic clipboard item 1/,
    });
    expect(within(once).queryByText('×1')).not.toBeInTheDocument();
    expect(within(once).queryByLabelText(/Captured \d+ times/)).not.toBeInTheDocument();
  });

  /// A fresh path per icon test: the icon cache is module-wide, and a path
  /// another test already answered would short-circuit the gateway mock.
  const freshApp = (): AppEntry => ({
    name: 'Iconed',
    bundleId: 'com.example.iconed',
    path: `/synthetic/Applications/${crypto.randomUUID()}/Iconed.app`,
  });

  const renderIconedList = (getAppIcon: ClipboardGateway['getAppIcon'], app: AppEntry) =>
    render(
      <GatewayProvider gateway={{ getAppIcon } as unknown as ClipboardGateway}>
        <PaletteList
          items={buildPaletteItems([app], [], '')}
          selectedKey={null}
          onSelect={() => undefined}
          onActivate={() => undefined}
        />
      </GatewayProvider>,
    );

  it('draws the rendered application icon in the row', async () => {
    const getAppIcon = vi.fn(async () => SYNTHETIC_APP_ICON);

    renderIconedList(getAppIcon, freshApp());

    const option = paletteList().getByRole('option');
    await waitFor(() => {
      const icon = option.querySelector('img.app-row__icon');
      expect(icon).not.toBeNull();
      expect(icon?.getAttribute('src')).toMatch(/^data:image\/png;base64,/);
    });
    // Decorative: the row's accessible name is the application, not a
    // repeated "icon of X" the screen reader would read twice.
    expect(option.querySelector('img.app-row__icon')).toHaveAttribute('alt', '');
  });

  it('keeps the glyph when no icon arrives', async () => {
    const getAppIcon = vi.fn(async () => null);

    renderIconedList(getAppIcon, freshApp());

    await waitFor(() => expect(getAppIcon).toHaveBeenCalled());
    // The placeholder glyph is the icon slot's occupant, not an <img>.
    expect(paletteList().getByRole('option').querySelector('img')).toBeNull();
  });
});
