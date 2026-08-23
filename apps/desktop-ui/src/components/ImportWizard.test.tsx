import '@testing-library/jest-dom/vitest';
import { act, fireEvent, render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, describe, expect, it, vi } from 'vitest';

import type {
  AppSettings,
  ImportAnalysis as ImportAnalysisContract,
  ImportProgress as ImportProgressContract,
  StorageStats,
} from '../lib/contracts';
import type { ClipboardGateway } from '../lib/gateway';
import { ImportProgress } from './ImportProgress';
import { ImportWizard } from './ImportWizard';

const RUN_ID = '0198f000-0000-7000-8000-000000000601';

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

const analysis: ImportAnalysisContract = {
  analysisId: RUN_ID,
  total: 6_503,
  candidateRecords: 6_500,
  skipped: 0,
  failed: 3,
};

const runningProgress = (processed = 250): ImportProgressContract => ({
  runId: RUN_ID,
  state: 'running',
  processed,
  total: 6_503,
  imported: processed,
  alreadyPresent: 0,
  skipped: 0,
  failed: 0,
  errorCode: null,
  summary: null,
});

const completedProgress: ImportProgressContract = {
  runId: RUN_ID,
  state: 'completed',
  processed: 6_503,
  total: 6_503,
  imported: 6_200,
  alreadyPresent: 250,
  skipped: 40,
  failed: 13,
  errorCode: null,
  summary: {
    runId: RUN_ID,
    total: 6_503,
    imported: 6_200,
    alreadyPresent: 250,
    skipped: 40,
    failed: 13,
  },
};

const settings: AppSettings = {
  schemaVersion: 1,
  hotkey: 'CommandOrControl+Shift+V',
  autostart: false,
  retentionDays: 30,
  denylistedApps: [],
};

const stats: StorageStats = {
  contentCount: 1,
  eventCount: 1,
  databaseBytes: 1,
  blobBytes: 0,
};

const makeGateway = (overrides: Partial<ClipboardGateway> = {}): ClipboardGateway =>
  ({
    search: vi.fn(async () => ({ items: [], nextCursor: null, rankedTruncated: false })),
    preview: vi.fn(async () => {
      throw new Error('unused');
    }),
    setPinned: vi.fn(async () => undefined),
    deleteEvent: vi.fn(async () => undefined),
    copyEvent: vi.fn(async (_eventId, plainText) => ({ mode: 'copied', plainText })),
    chooseImportFile: vi.fn(async () => '/synthetic/export.json'),
    chooseImportDirectory: vi.fn(async () => '/synthetic/export'),
    analyzeImport: vi.fn(async () => analysis),
    startImport: vi.fn(async () => ({ runId: RUN_ID })),
    discardImportAnalysis: vi.fn(async () => undefined),
    getImportStatus: vi.fn(async () => completedProgress),
    revealSource: vi.fn(async () => undefined),
    getThumbnail: vi.fn(async () => null),
    getSettings: vi.fn(async () => settings),
    saveSettings: vi.fn(async (nextSettings) => nextSettings),
    isAutostartEnabled: vi.fn(async () => false),
    setAutostartEnabled: vi.fn(async () => undefined),
    getStorageStats: vi.fn(async () => stats),
    ...overrides,
  }) as ClipboardGateway;

const chooseFile = async (user: ReturnType<typeof userEvent.setup>): Promise<void> => {
  await user.click(screen.getByRole('button', { name: 'Wybierz plik JSON' }));
  await screen.findByRole('button', { name: 'Rozpocznij import' });
};

afterEach(() => {
  vi.useRealTimers();
  vi.restoreAllMocks();
});

describe('ImportWizard privacy and confirmation', () => {
  it('does not analyze when the native picker is cancelled', async () => {
    const analyzeImport = vi.fn<ClipboardGateway['analyzeImport']>();
    const gateway = makeGateway({
      chooseImportFile: vi.fn(async () => null),
      analyzeImport,
    });
    const user = userEvent.setup();
    render(<ImportWizard gateway={gateway} />);

    expect(screen.getByRole('button', { name: 'Wybierz plik JSON' })).toHaveFocus();
    await user.click(screen.getByRole('button', { name: 'Wybierz plik JSON' }));

    expect(analyzeImport).not.toHaveBeenCalled();
    expect(screen.getByRole('button', { name: 'Wybierz plik JSON' })).toBeVisible();
  });

  it('passes the selected path once and never renders or sends it after analysis', async () => {
    const privatePath = '/Users/private/fixture secret payload/export.json';
    const analyzeImport = vi.fn(async () => analysis);
    const startImport = vi.fn(async () => ({ runId: RUN_ID }));
    const getImportStatus = vi.fn(async () => completedProgress);
    const gateway = makeGateway({
      chooseImportFile: vi.fn(async () => privatePath),
      analyzeImport,
      startImport,
      getImportStatus,
    });
    const user = userEvent.setup();
    const { container } = render(<ImportWizard gateway={gateway} pollIntervalMs={5} />);

    await chooseFile(user);
    expect(screen.getByText('6 503 rekordów')).toBeVisible();
    expect(container).not.toHaveTextContent(privatePath);
    expect(container).not.toHaveTextContent('fixture secret payload');

    await user.click(screen.getByRole('button', { name: 'Rozpocznij import' }));
    expect(await screen.findByRole('heading', { name: 'Import zakończony' })).toBeVisible();
    expect(analyzeImport).toHaveBeenCalledOnce();
    expect(analyzeImport).toHaveBeenCalledWith(privatePath);
    expect(startImport).toHaveBeenCalledWith(RUN_ID);
    expect(getImportStatus).toHaveBeenCalledWith(RUN_ID);
    expect(JSON.stringify(startImport.mock.calls)).not.toContain(privatePath);
    expect(JSON.stringify(getImportStatus.mock.calls)).not.toContain(privatePath);
    expect(container).not.toHaveTextContent(privatePath);
  });

  it('renders a path-free generic analysis failure', async () => {
    const gateway = makeGateway({
      chooseImportDirectory: vi.fn(async () => '/private/archive/clipboard.json'),
      analyzeImport: vi.fn(async () => {
        throw new Error('/private/archive/clipboard.json contains secret-token');
      }),
    });
    const user = userEvent.setup();
    const { container } = render(<ImportWizard gateway={gateway} />);

    await user.click(screen.getByRole('button', { name: 'Wybierz katalog eksportu' }));

    expect(await screen.findByRole('alert')).toHaveTextContent(
      'Nie udało się przeanalizować archiwum',
    );
    expect(container).not.toHaveTextContent('/private/archive');
    expect(container).not.toHaveTextContent('secret-token');
  });

  it('awaits analysis discard before closing confirmation', async () => {
    const discard = deferred<void>();
    const onClose = vi.fn();
    const gateway = makeGateway({ discardImportAnalysis: vi.fn(() => discard.promise) });
    const user = userEvent.setup();
    render(<ImportWizard gateway={gateway} onClose={onClose} />);
    await chooseFile(user);

    await user.click(screen.getByRole('button', { name: 'Anuluj import' }));
    expect(screen.getByRole('button', { name: 'Anulowanie…' })).toBeDisabled();
    expect(onClose).not.toHaveBeenCalled();

    await act(async () => discard.resolve());
    expect(onClose).toHaveBeenCalledOnce();
    expect(gateway.discardImportAnalysis).toHaveBeenCalledWith(RUN_ID);
  });

  it('stays open with a sanitized message when discard fails', async () => {
    const onClose = vi.fn();
    const gateway = makeGateway({
      discardImportAnalysis: vi.fn(async () => {
        throw new Error('/private/export.json could not be discarded');
      }),
    });
    const user = userEvent.setup();
    const { container } = render(<ImportWizard gateway={gateway} onClose={onClose} />);
    await chooseFile(user);

    await user.click(screen.getByRole('button', { name: 'Anuluj import' }));

    expect(await screen.findByRole('alert')).toHaveTextContent(
      'Nie udało się anulować przygotowanego importu',
    );
    expect(screen.getByRole('button', { name: 'Rozpocznij import' })).toBeVisible();
    expect(container).not.toHaveTextContent('/private/export.json');
    expect(onClose).not.toHaveBeenCalled();
  });

  it('routes Escape and backdrop cancellation through the awaited discard', async () => {
    const discardImportAnalysis = vi.fn(async () => undefined);
    const onClose = vi.fn();
    const user = userEvent.setup();
    const { rerender } = render(
      <ImportWizard gateway={makeGateway({ discardImportAnalysis })} onClose={onClose} />,
    );
    await chooseFile(user);

    await user.keyboard('{Escape}');
    await waitFor(() => expect(onClose).toHaveBeenCalledOnce());
    expect(discardImportAnalysis).toHaveBeenCalledWith(RUN_ID);

    onClose.mockClear();
    discardImportAnalysis.mockClear();
    rerender(<ImportWizard gateway={makeGateway({ discardImportAnalysis })} onClose={onClose} />);
    await chooseFile(user);
    fireEvent.mouseDown(screen.getByTestId('import-backdrop'));
    await waitFor(() => expect(onClose).toHaveBeenCalledOnce());
    expect(discardImportAnalysis).toHaveBeenCalledWith(RUN_ID);
  });
});

describe('ImportWizard start recovery and polling', () => {
  it('rejects a mismatched start run id without polling or discarding', async () => {
    const getImportStatus = vi.fn<ClipboardGateway['getImportStatus']>();
    const discardImportAnalysis = vi.fn(async () => undefined);
    const gateway = makeGateway({
      startImport: vi.fn(async () => ({ runId: 'different-run' })),
      getImportStatus,
      discardImportAnalysis,
    });
    const user = userEvent.setup();
    render(<ImportWizard gateway={gateway} />);
    await chooseFile(user);

    await user.click(screen.getByRole('button', { name: 'Rozpocznij import' }));

    expect(await screen.findByRole('heading', { name: 'Import nie został ukończony' })).toBeVisible();
    expect(getImportStatus).not.toHaveBeenCalled();
    expect(discardImportAnalysis).not.toHaveBeenCalled();
  });

  it('recovers a lost start response by polling the analysis id', async () => {
    const getImportStatus = vi
      .fn<ClipboardGateway['getImportStatus']>()
      .mockResolvedValueOnce(runningProgress())
      .mockResolvedValueOnce(completedProgress);
    const gateway = makeGateway({
      startImport: vi.fn(async () => {
        throw new Error('/private/archive.json response lost');
      }),
      getImportStatus,
    });
    const user = userEvent.setup();
    const { container } = render(<ImportWizard gateway={gateway} pollIntervalMs={5} />);
    await chooseFile(user);

    await user.click(screen.getByRole('button', { name: 'Rozpocznij import' }));

    expect(await screen.findByRole('heading', { name: 'Import zakończony' })).toBeVisible();
    expect(getImportStatus).toHaveBeenNthCalledWith(1, RUN_ID);
    expect(getImportStatus).toHaveBeenNthCalledWith(2, RUN_ID);
    expect(container).not.toHaveTextContent('archive.json');
    expect(gateway.discardImportAnalysis).not.toHaveBeenCalled();
  });

  it('never overlaps status requests and stops polling after a terminal response', async () => {
    vi.useFakeTimers();
    const first = deferred<ImportProgressContract>();
    const getImportStatus = vi
      .fn<ClipboardGateway['getImportStatus']>()
      .mockImplementationOnce(() => first.promise)
      .mockResolvedValueOnce(completedProgress);
    const gateway = makeGateway({ getImportStatus });
    const { unmount } = render(<ImportWizard gateway={gateway} pollIntervalMs={50} />);

    fireEvent.click(screen.getByRole('button', { name: 'Wybierz plik JSON' }));
    await act(async () => Promise.resolve());
    fireEvent.click(screen.getByRole('button', { name: 'Rozpocznij import' }));
    await act(async () => Promise.resolve());
    expect(getImportStatus).toHaveBeenCalledOnce();

    await act(async () => vi.advanceTimersByTimeAsync(500));
    expect(getImportStatus).toHaveBeenCalledOnce();

    await act(async () => first.resolve(runningProgress()));
    expect(screen.getByRole('progressbar', { name: 'Postęp importu' })).toBeVisible();
    await act(async () => vi.advanceTimersByTimeAsync(50));
    expect(getImportStatus).toHaveBeenCalledTimes(2);
    expect(screen.getByRole('heading', { name: 'Import zakończony' })).toBeVisible();

    await act(async () => vi.advanceTimersByTimeAsync(500));
    expect(getImportStatus).toHaveBeenCalledTimes(2);
    unmount();
  });

  it('ignores a pending status response after unmount', async () => {
    const request = deferred<ImportProgressContract>();
    const getImportStatus = vi.fn(() => request.promise);
    const gateway = makeGateway({ getImportStatus });
    const { unmount } = render(<ImportWizard gateway={gateway} pollIntervalMs={5} />);

    fireEvent.click(screen.getByRole('button', { name: 'Wybierz plik JSON' }));
    await screen.findByRole('button', { name: 'Rozpocznij import' });
    fireEvent.click(screen.getByRole('button', { name: 'Rozpocznij import' }));
    await waitFor(() => expect(getImportStatus).toHaveBeenCalledOnce());
    unmount();

    await act(async () => request.resolve(completedProgress));
    expect(screen.queryByRole('heading', { name: 'Import zakończony' })).not.toBeInTheDocument();
  });

  it('keeps polling failures private and accepts only a matching progress run id', async () => {
    const getImportStatus = vi
      .fn<ClipboardGateway['getImportStatus']>()
      .mockRejectedValueOnce(new Error('/private/export.json unavailable'))
      .mockResolvedValueOnce({ ...completedProgress, runId: 'different-run' });
    const gateway = makeGateway({ getImportStatus });
    const user = userEvent.setup();
    const { container } = render(<ImportWizard gateway={gateway} pollIntervalMs={5} />);
    await chooseFile(user);

    await user.click(screen.getByRole('button', { name: 'Rozpocznij import' }));

    expect(await screen.findByRole('heading', { name: 'Import nie został ukończony' })).toBeVisible();
    expect(container).not.toHaveTextContent('/private/export.json');
    expect(container).not.toHaveTextContent('different-run');
  });
});

describe('Import dialog accessibility', () => {
  it('is labelled, describes the privacy warning, traps focus, and starts on the primary action', async () => {
    const user = userEvent.setup();
    render(<ImportWizard gateway={makeGateway()} />);

    const dialog = screen.getByRole('dialog', { name: 'Importuj historię' });
    expect(dialog).toHaveAttribute('aria-modal', 'true');
    expect(dialog).toHaveAccessibleDescription(/archiwum może zawierać dane wrażliwe/i);
    const primary = screen.getByRole('button', { name: 'Wybierz plik JSON' });
    const close = screen.getByRole('button', { name: 'Zamknij import' });
    expect(primary).toHaveFocus();

    close.focus();
    await user.tab();
    expect(primary).toHaveFocus();
    await user.tab({ shift: true });
    expect(close).toHaveFocus();
  });

  it('exposes bounded count-only progress and a polite live region', () => {
    const { container } = render(
      <ImportProgress progress={runningProgress(250)} phase="running" />,
    );

    const progressbar = screen.getByRole('progressbar', { name: 'Postęp importu' });
    expect(progressbar).toHaveAttribute('aria-valuemin', '0');
    expect(progressbar).toHaveAttribute('aria-valuemax', '6503');
    expect(progressbar).toHaveAttribute('aria-valuenow', '250');
    expect(screen.getByText('250 z 6 503 rekordów')).toHaveAttribute('aria-live', 'polite');
    expect(container).not.toHaveTextContent('payload');
    expect(container).not.toHaveTextContent('/');
  });
});
