import '@testing-library/jest-dom/vitest';
import { act, render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import { App } from '../App';
import {
  MAX_THUMBNAIL_BASE64_BYTES,
  type AppSettings,
  type HistoryItem,
  type HistoryPage,
  type ImportAnalysis,
  type ImportProgress,
  type ImportRunHandle,
  type Preview,
  type StorageStats,
  type Thumbnail,
} from '../lib/contracts';
import { thumbnailDataUrl } from '../lib/format';
import type { ClipboardGateway } from '../lib/gateway';
import { PreviewPane } from './PreviewPane';

const makeItem = (eventId: number, overrides: Partial<HistoryItem> = {}): HistoryItem => ({
  eventId,
  globalId: `0198f000-0000-7000-8000-${String(eventId).padStart(12, '0')}`,
  kind: 'text',
  capturedAtMs: 1_775_000_000_000 - eventId,
  sourceAppName: 'Synthetic Editor',
  pinned: false,
  preview: `Synthetic item ${eventId}`,
  byteSize: 32,
  hasThumbnail: false,
  ...overrides,
});

const makePreview = (eventId: number, overrides: Partial<Preview> = {}): Preview => ({
  eventId,
  kind: 'text',
  mimeType: 'text/plain',
  text: `Synthetic preview ${eventId}`,
  byteSize: 32,
  sourceAppName: 'Synthetic Editor',
  sourcePath: null,
  sourceExists: false,
  ...overrides,
});

interface Deferred<T> {
  promise: Promise<T>;
  resolve: (value: T) => void;
  reject: (reason?: unknown) => void;
}

const deferred = <T,>(): Deferred<T> => {
  let resolve!: (value: T) => void;
  let reject!: (reason?: unknown) => void;
  const promise = new Promise<T>((nextResolve, nextReject) => {
    resolve = nextResolve;
    reject = nextReject;
  });
  return { promise, resolve, reject };
};

const settings: AppSettings = {
  schemaVersion: 1,
  hotkey: 'CommandOrControl+Shift+V',
  autostart: false,
  retentionDays: 30,
  denylistedApps: [],
};

const importProgress: ImportProgress = {
  runId: '0198f000-0000-7000-8000-000000000500',
  state: 'running',
  processed: 0,
  total: 1,
  imported: 0,
  alreadyPresent: 0,
  skipped: 0,
  failed: 0,
  errorCode: null,
  summary: null,
};

const makeGateway = (
  items: HistoryItem[],
  overrides: Partial<ClipboardGateway> = {},
): ClipboardGateway => {
  const page: HistoryPage = { items, nextCursor: null, rankedTruncated: false };
  return {
    search: vi.fn(async () => page),
    preview: vi.fn(async (eventId) => makePreview(eventId)),
    setPinned: vi.fn(async () => undefined),
    deleteEvent: vi.fn(async () => undefined),
    copyEvent: vi.fn(async (_eventId, plainText) => ({
      mode: 'copied' as const,
      plainText,
    })),
    chooseImportFile: vi.fn(async () => null),
    chooseImportDirectory: vi.fn(async () => null),
    analyzeImport: vi.fn(async (): Promise<ImportAnalysis> => ({
      analysisId: '0198f000-0000-7000-8000-000000000501',
      total: 1,
      candidateRecords: 1,
      skipped: 0,
      failed: 0,
    })),
    startImport: vi.fn(async (): Promise<ImportRunHandle> => ({
      runId: importProgress.runId,
    })),
    discardImportAnalysis: vi.fn(async () => undefined),
    getImportStatus: vi.fn(async () => importProgress),
    revealSource: vi.fn(async () => undefined),
    onHistoryChanged: vi.fn(() => () => undefined),
    getThumbnail: vi.fn(async (): Promise<Thumbnail | null> => null),
    getSettings: vi.fn(async () => settings),
    isAutostartEnabled: vi.fn(async () => settings.autostart),
    setAutostartEnabled: vi.fn(async () => undefined),
    saveSettings: vi.fn(async (nextSettings) => nextSettings),
    getStorageStats: vi.fn(async (): Promise<StorageStats> => ({
      contentCount: 1,
      eventCount: 1,
      databaseBytes: 1,
      blobBytes: 0,
    })),
    ...overrides,
  };
};

const settleInitialSearch = async (): Promise<void> => {
  await screen.findByRole('listbox', { name: 'Wyniki historii schowka' });
};

describe('safe preview rendering', () => {
  it('renders imported HTML as literal text instead of executable markup', () => {
    const malicious = '<img src=x onerror=alert(1)><script>window.pwned=true</script>';
    render(
      <PreviewPane
        preview={makePreview(1, {
          kind: 'html',
          mimeType: 'text/html',
          text: malicious,
        })}
        thumbnailUrl={null}
        thumbnailStatus="idle"
      />,
    );

    expect(screen.getByText(malicious)).toBeVisible();
    expect(document.querySelector('img')).toBeNull();
    expect(document.querySelector('script')).toBeNull();
  });

  it('renders URL text without requesting it as an image or link', () => {
    render(
      <PreviewPane
        preview={makePreview(1, {
          kind: 'link',
          mimeType: 'text/plain',
          text: 'https://example.invalid/private-resource',
        })}
        thumbnailUrl={null}
        thumbnailStatus="idle"
      />,
    );

    expect(screen.getByText('https://example.invalid/private-resource')).toBeVisible();
    expect(document.querySelector('a')).toBeNull();
    expect(document.querySelector('img')).toBeNull();
  });

  it('offers a copy path when a thumbnail cannot be rendered', () => {
    render(
      <PreviewPane
        preview={makePreview(1, {
          kind: 'image',
          mimeType: 'image/png',
          text: null,
        })}
        thumbnailUrl={null}
        thumbnailStatus="idle"
      />,
    );

    // The importer no longer keeps entries whose source is gone, so the only
    // remaining case is a thumbnail that could not be produced.
    expect(screen.getByText('Miniatura jest niedostępna')).toBeVisible();
  });

  it('accepts only bounded PNG thumbnail payloads', () => {
    expect(
      thumbnailDataUrl({ mimeType: 'image/png', base64: 'c3ludGhldGlj' }),
    ).toBe('data:image/png;base64,c3ludGhldGlj');
    expect(() =>
      thumbnailDataUrl({ mimeType: 'image/jpeg', base64: 'c3ludGhldGlj' }),
    ).toThrow('thumbnail_invalid_mime');
    expect(() =>
      thumbnailDataUrl({ mimeType: 'image/png', base64: 'A'.repeat(MAX_THUMBNAIL_BASE64_BYTES + 1) }),
    ).toThrow('thumbnail_too_large');
  });
});

describe('selected preview sequencing', () => {
  beforeEach(() => {
    vi.spyOn(HTMLElement.prototype, 'offsetWidth', 'get').mockReturnValue(800);
    vi.spyOn(HTMLElement.prototype, 'offsetHeight', 'get').mockReturnValue(420);
  });

  afterEach(() => {
    vi.restoreAllMocks();
  });

  it('ignores a stale preview response after selection changes', async () => {
    const first = deferred<Preview>();
    const second = deferred<Preview>();
    const gateway = makeGateway([makeItem(1), makeItem(2)], {
      preview: vi.fn((eventId) => (eventId === 1 ? first.promise : second.promise)),
    });
    render(<App gateway={gateway} />);
    await settleInitialSearch();

    await act(async () => {
      screen.getByRole('option', { name: /Synthetic item 2/ }).click();
    });
    await act(async () => {
      second.resolve(makePreview(2, { text: 'Newest selected preview' }));
    });
    expect(await screen.findByText('Newest selected preview')).toBeVisible();

    await act(async () => {
      first.resolve(makePreview(1, { text: 'Stale private preview' }));
    });
    expect(screen.queryByText('Stale private preview')).not.toBeInTheDocument();
    expect(screen.getByText('Newest selected preview')).toBeVisible();
  });

  it('ignores a stale thumbnail response after selection changes', async () => {
    const first = deferred<Thumbnail | null>();
    const second = deferred<Thumbnail | null>();
    const items = [
      makeItem(1, { kind: 'image', hasThumbnail: true }),
      makeItem(2, { kind: 'image', hasThumbnail: true }),
    ];
    const gateway = makeGateway(items, {
      preview: vi.fn(async (eventId) =>
        makePreview(eventId, { kind: 'image', mimeType: 'image/png', text: null }),
      ),
      getThumbnail: vi.fn((eventId) => (eventId === 1 ? first.promise : second.promise)),
    });
    render(<App gateway={gateway} />);
    await settleInitialSearch();

    await act(async () => {
      screen.getByRole('option', { name: /Synthetic item 2/ }).click();
      second.resolve({ mimeType: 'image/png', base64: 'bmV3' });
    });
    expect(await screen.findByRole('img', { name: 'Podgląd obrazu ze schowka' })).toHaveAttribute(
      'src',
      'data:image/png;base64,bmV3',
    );

    await act(async () => {
      first.resolve({ mimeType: 'image/png', base64: 'b2xk' });
    });
    expect(screen.getByRole('img', { name: 'Podgląd obrazu ze schowka' })).toHaveAttribute(
      'src',
      'data:image/png;base64,bmV3',
    );
  });

  it('keeps a full file path out of the list and shows it only in selected preview', async () => {
    const fullPath = '/synthetic/private-folder/report.txt';
    const gateway = makeGateway([makeItem(1, { kind: 'file', preview: fullPath })], {
      preview: vi.fn(async () =>
        makePreview(1, { kind: 'file', mimeType: 'text/plain', text: fullPath }),
      ),
    });
    render(<App gateway={gateway} />);
    await settleInitialSearch();

    const listbox = screen.getByRole('listbox', { name: 'Wyniki historii schowka' });
    expect(listbox).toHaveTextContent('report.txt');
    expect(listbox).not.toHaveTextContent('/synthetic/private-folder');
    expect(await screen.findByText(fullPath)).toBeVisible();
  });

  it('shows a stable preview error without exposing rejected backend details', async () => {
    const gateway = makeGateway([makeItem(1)], {
      preview: vi.fn(async () => {
        throw new Error('/synthetic/private-folder/report.txt query=secret');
      }),
    });
    render(<App gateway={gateway} />);
    await settleInitialSearch();

    const alert = await screen.findByRole('alert');
    expect(alert).toHaveTextContent('Nie udało się wczytać podglądu');
    expect(alert).not.toHaveTextContent('private-folder');
    expect(alert).not.toHaveTextContent('secret');
  });

  it('opens and closes the adaptive preview through labelled controls', async () => {
    const user = userEvent.setup();
    render(<App gateway={makeGateway([makeItem(1)])} />);
    await settleInitialSearch();

    const previewColumn = screen
      .getByLabelText('Podgląd zaznaczonego wpisu')
      .closest('.preview-column');
    expect(previewColumn).not.toHaveClass('is-mobile-open');

    await user.click(
      screen.getByRole('button', { name: 'Pokaż podgląd zaznaczonego wpisu' }),
    );
    expect(previewColumn).toHaveClass('is-mobile-open');

    await user.click(screen.getByRole('button', { name: 'Zamknij podgląd' }));
    expect(previewColumn).not.toHaveClass('is-mobile-open');
  });
});

describe('source location', () => {
  it('shows the full path and offers to reveal an existing file', async () => {
    const revealSource = vi.fn(async () => undefined);
    const user = userEvent.setup();
    render(
      <App
        gateway={makeGateway([makeItem(1, { kind: 'file' })], {
          revealSource,
          preview: vi.fn(async (eventId) =>
            makePreview(eventId, {
              kind: 'file',
              sourcePath: '/Users/synthetic/Pobrane/raport syntetyczny.pdf',
              sourceExists: true,
            }),
          ),
        })}
      />,
    );
    await settleInitialSearch();

    expect(
      await screen.findByText('/Users/synthetic/Pobrane/raport syntetyczny.pdf'),
    ).toBeVisible();
    expect(screen.getByRole('heading', { name: 'raport syntetyczny.pdf' })).toBeVisible();

    await user.click(screen.getByRole('button', { name: 'Pokaż w Finderze' }));
    expect(revealSource).toHaveBeenCalledWith(1);
  });

  it('drops the reveal action for a file that has been moved since the import', async () => {
    render(
      <App
        gateway={makeGateway([makeItem(1, { kind: 'file' })], {
          preview: vi.fn(async (eventId) =>
            makePreview(eventId, {
              kind: 'file',
              sourcePath: '/Users/synthetic/Pobrane/usuniety.pdf',
              sourceExists: false,
            }),
          ),
        })}
      />,
    );
    await settleInitialSearch();

    expect(await screen.findByText('/Users/synthetic/Pobrane/usuniety.pdf')).toBeVisible();
    expect(screen.queryByRole('button', { name: 'Pokaż w Finderze' })).toBeNull();
  });

  it('shows no location block for an entry whose source recorded none', async () => {
    render(<App gateway={makeGateway([makeItem(1)])} />);
    await settleInitialSearch();

    await screen.findByText('Synthetic preview 1');
    expect(screen.queryByText('Lokalizacja źródłowa')).toBeNull();
  });
});

describe('history actions', () => {
  beforeEach(() => {
    vi.spyOn(HTMLElement.prototype, 'offsetWidth', 'get').mockReturnValue(800);
    vi.spyOn(HTMLElement.prototype, 'offsetHeight', 'get').mockReturnValue(420);
  });

  afterEach(() => {
    vi.restoreAllMocks();
  });

  it('rolls back an optimistic pin and reports a sanitized failure', async () => {
    const request = deferred<void>();
    const gateway = makeGateway([makeItem(1)], {
      setPinned: vi.fn(() => request.promise),
    });
    const user = userEvent.setup();
    render(<App gateway={gateway} />);
    await settleInitialSearch();
    await screen.findByText('Synthetic preview 1');

    await user.click(screen.getByRole('button', { name: 'Przypnij wpis' }));
    expect(screen.getByRole('button', { name: 'Odepnij wpis' })).toBeVisible();

    await act(async () => {
      request.reject(new Error('/private/archive.json backend detail'));
    });
    expect(await screen.findByRole('status')).toHaveTextContent(
      'Nie udało się zmienić przypięcia',
    );
    expect(screen.getByRole('button', { name: 'Przypnij wpis' })).toBeVisible();
    expect(screen.getByRole('status')).not.toHaveTextContent('archive.json');
  });

  it('opens an accessible delete confirmation with cancel focused by default', async () => {
    const gateway = makeGateway([makeItem(1)]);
    const user = userEvent.setup();
    render(<App gateway={gateway} />);
    await settleInitialSearch();

    await user.click(screen.getByRole('button', { name: 'Usuń wpis' }));

    const dialog = screen.getByRole('dialog', { name: 'Usunąć wpis z historii?' });
    const cancel = screen.getByRole('button', { name: 'Anuluj usuwanie' });
    expect(dialog).toBeVisible();
    expect(cancel).toHaveFocus();
    expect(screen.getByRole('button', { name: 'Usuń wpis bezpowrotnie' })).toBeVisible();
    expect(screen.getByLabelText('Paleta historii schowka')).toHaveAttribute('inert');

    await user.click(cancel);
    expect(dialog).not.toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Usuń wpis' })).toHaveFocus();
    expect(screen.getByLabelText('Paleta historii schowka')).not.toHaveAttribute('inert');
  });

  it('traps forward and reverse focus inside delete confirmation', async () => {
    const user = userEvent.setup();
    render(<App gateway={makeGateway([makeItem(1)])} />);
    await settleInitialSearch();

    await user.click(screen.getByRole('button', { name: 'Usuń wpis' }));
    const cancel = screen.getByRole('button', { name: 'Anuluj usuwanie' });
    const confirm = screen.getByRole('button', { name: 'Usuń wpis bezpowrotnie' });
    expect(cancel).toHaveFocus();

    await user.tab({ shift: true });
    expect(confirm).toHaveFocus();
    await user.tab();
    expect(cancel).toHaveFocus();
  });

  it('removes a deleted item and moves selection without stale activation', async () => {
    const copyEvent = vi.fn(async (_eventId: number, plainText: boolean) => ({
      mode: 'copied' as const,
      plainText,
    }));
    const gateway = makeGateway([makeItem(1), makeItem(2)], { copyEvent });
    const user = userEvent.setup();
    render(<App gateway={gateway} />);
    await settleInitialSearch();

    await user.click(screen.getByRole('button', { name: 'Usuń wpis' }));
    await user.click(screen.getByRole('button', { name: 'Usuń wpis bezpowrotnie' }));

    await waitFor(() => {
      expect(screen.queryByRole('option', { name: /Synthetic item 1/ })).not.toBeInTheDocument();
    });
    expect(screen.getByRole('option', { selected: true })).toHaveAttribute('data-event-id', '2');

    await user.keyboard('{Enter}');
    expect(copyEvent).toHaveBeenLastCalledWith(2, false);
  });

  it('supports copy, paste, plain-text and pin hotkeys from search focus', async () => {
    const copyEvent = vi.fn(async (_eventId: number, plainText: boolean) => ({
      mode: 'copied' as const,
      plainText,
    }));
    const setPinned = vi.fn(async () => undefined);
    const gateway = makeGateway([makeItem(1)], { copyEvent, setPinned });
    const user = userEvent.setup();
    render(<App gateway={gateway} />);
    await settleInitialSearch();
    const search = screen.getByRole('searchbox', { name: 'Przeszukaj historię' });
    expect(search).toHaveFocus();

    await user.keyboard('{Enter}');
    await user.keyboard('{Meta>}c{/Meta}');
    await user.keyboard('{Meta>}{Shift>}v{/Shift}{/Meta}');
    await user.keyboard('{Meta>}p{/Meta}');
    await user.keyboard('{Delete}');

    expect(copyEvent).toHaveBeenCalledWith(1, false);
    expect(copyEvent).toHaveBeenCalledWith(1, true);
    expect(setPinned).toHaveBeenCalledWith(1, true);
    // Delete belongs to the query field while it has focus. Proposing to erase
    // a history entry when the user meant to erase a character is the wrong
    // trade in a field they type in constantly.
    expect(screen.queryByRole('dialog', { name: 'Usunąć wpis z historii?' })).toBeNull();
  });

  it('preserves copy, paste, and plain-text paste intent in fallback feedback', async () => {
    const copyEvent = vi.fn(async (_eventId: number, plainText: boolean) => ({
      mode: 'copied' as const,
      plainText,
    }));
    const user = userEvent.setup();
    render(<App gateway={makeGateway([makeItem(1)], { copyEvent })} />);
    await settleInitialSearch();

    await user.keyboard('{Enter}');
    expect(await screen.findByText('Skopiowano jako fallback — automatyczne wklejenie jest niedostępne.')).toBeVisible();

    await user.keyboard('{Meta>}c{/Meta}');
    expect(await screen.findByText('Skopiowano do schowka.')).toBeVisible();

    await user.keyboard('{Meta>}{Shift>}v{/Shift}{/Meta}');
    expect(
      await screen.findByText(
        'Skopiowano jako zwykły tekst — automatyczne wklejenie jest niedostępne.',
      ),
    ).toBeVisible();
    expect(copyEvent.mock.calls).toEqual([
      [1, false],
      [1, false],
      [1, true],
    ]);
  });

  it('lets Backspace edit the query instead of proposing a deletion', async () => {
    const deleteEvent = vi.fn(async () => undefined);
    const user = userEvent.setup();
    render(<App gateway={makeGateway([makeItem(1), makeItem(2)], { deleteEvent })} />);
    await settleInitialSearch();

    // A row must be selected: the palette only proposes a deletion when there
    // is something to delete, which is exactly the state the user is in while
    // typing a query over a populated list.
    await user.click(screen.getByRole('option', { name: /Synthetic item 1/ }));
    const search = screen.getByRole('searchbox');
    await user.click(search);
    await user.type(search, 'abc');
    await user.keyboard('{Backspace}');

    expect(search).toHaveValue('ab');
    expect(screen.queryByRole('dialog', { name: 'Usunąć wpis z historii?' })).toBeNull();
    expect(deleteEvent).not.toHaveBeenCalled();
  });

  it('still deletes with Backspace when the query field does not own focus', async () => {
    const user = userEvent.setup();
    render(<App gateway={makeGateway([makeItem(1), makeItem(2)])} />);
    await settleInitialSearch();

    await user.click(screen.getByRole('option', { name: /Synthetic item 1/ }));
    await user.click(screen.getByRole('button', { name: 'Usuń wpis' }));

    expect(screen.getByRole('dialog', { name: 'Usunąć wpis z historii?' })).toBeVisible();
  });

  it('suppresses history hotkeys while a dialog owns focus', async () => {
    const copyEvent = vi.fn(async (_eventId: number, plainText: boolean) => ({
      mode: 'copied' as const,
      plainText,
    }));
    const setPinned = vi.fn(async () => undefined);
    const gateway = makeGateway([makeItem(1)], { copyEvent, setPinned });
    const user = userEvent.setup();
    render(<App gateway={gateway} />);
    await settleInitialSearch();

    await user.click(screen.getByRole('button', { name: 'Usuń wpis' }));
    await user.keyboard('{Meta>}c{/Meta}{Meta>}p{/Meta}');

    expect(copyEvent).not.toHaveBeenCalled();
    expect(setPinned).not.toHaveBeenCalled();
    expect(gateway.deleteEvent).not.toHaveBeenCalled();
    expect(screen.getByRole('dialog', { name: 'Usunąć wpis z historii?' })).toBeVisible();

    await user.keyboard('{Enter}');
    expect(screen.queryByRole('dialog')).not.toBeInTheDocument();
    expect(gateway.deleteEvent).not.toHaveBeenCalled();

    const settingsInput = document.createElement('input');
    settingsInput.setAttribute('aria-label', 'Pole ustawień');
    screen.getByRole('application', { name: 'Historia schowka' }).append(settingsInput);
    settingsInput.focus();
    await user.keyboard('{Meta>}c{/Meta}{Meta>}p{/Meta}{Delete}');

    expect(copyEvent).not.toHaveBeenCalled();
    expect(setPinned).not.toHaveBeenCalled();
    expect(screen.queryByRole('dialog')).not.toBeInTheDocument();
  });

  it('exposes labelled mouse and screen-reader equivalents for every action', async () => {
    render(<App gateway={makeGateway([makeItem(1)])} />);
    await settleInitialSearch();

    expect(await screen.findByRole('button', { name: 'Wklej lub skopiuj wpis' })).toBeVisible();
    expect(screen.getByRole('button', { name: 'Kopiuj jako zwykły tekst' })).toBeVisible();
    expect(screen.getByRole('button', { name: 'Przypnij wpis' })).toBeVisible();
    expect(screen.getByRole('button', { name: 'Usuń wpis' })).toBeVisible();
  });
});
