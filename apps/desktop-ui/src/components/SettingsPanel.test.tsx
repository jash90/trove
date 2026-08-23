import '@testing-library/jest-dom/vitest';
import { act, fireEvent, render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { describe, expect, it, vi } from 'vitest';

import type { AppSettings, StorageStats } from '../lib/contracts';
import type { ClipboardGateway } from '../lib/gateway';
import {
  normalizeDenylistEntries,
  normalizeExecutableDenylistEntry,
  normalizePlatformHotkey,
  SettingsPanel,
} from './SettingsPanel';
import { StorageStats as StorageStatsView } from './StorageStats';

interface Deferred<T> {
  promise: Promise<T>;
  resolve: (value: T) => void;
}

const deferred = <T,>(): Deferred<T> => {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((nextResolve) => {
    resolve = nextResolve;
  });
  return { promise, resolve };
};

const persistedSettings: AppSettings = {
  schemaVersion: 1,
  hotkey: 'CommandOrControl+Shift+V',
  autostart: false,
  retentionDays: 30,
  denylistedApps: ['com.acme.private'],
};

const storageStats: StorageStats = {
  contentCount: 4,
  eventCount: 5,
  databaseBytes: 2_048,
  blobBytes: 4_096,
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
    chooseImportFile: vi.fn(async () => null),
    chooseImportDirectory: vi.fn(async () => null),
    analyzeImport: vi.fn(async () => {
      throw new Error('unused');
    }),
    startImport: vi.fn(async () => {
      throw new Error('unused');
    }),
    discardImportAnalysis: vi.fn(async () => undefined),
    getImportStatus: vi.fn(async () => {
      throw new Error('unused');
    }),
    revealSource: vi.fn(async () => undefined),
    onHistoryChanged: vi.fn(() => () => undefined),
    getThumbnail: vi.fn(async () => null),
    getSettings: vi.fn(async () => persistedSettings),
    saveSettings: vi.fn(async (nextSettings) => nextSettings),
    isAutostartEnabled: vi.fn(async () => false),
    setAutostartEnabled: vi.fn(async () => undefined),
    getStorageStats: vi.fn(async () => storageStats),
    ...overrides,
  }) as ClipboardGateway;

const loadSettings = async (): Promise<HTMLInputElement> =>
  screen.findByRole('textbox', { name: 'Skrót globalny' });

describe('settings normalization', () => {
  it.each([
    ['commandorcontrol + shift + space', 'CommandOrControl+Shift+Space'],
    ['commandorcontrol + shift + v', 'CommandOrControl+Shift+V'],
    ['control+alt+7', 'Control+Alt+7'],
    ['Command+Shift+F12', 'Command+Shift+F12'],
  ])('normalizes a conservative platform hotkey: %s', (input, expected) => {
    expect(normalizePlatformHotkey(input)).toBe(expected);
  });

  it.each([
    'V',
    'CommandOrControl+Command+V',
    'Control+Control+V',
    'CommandOrControl+Shift+?',
    'CommandOrControl+Hyper+V',
    'CommandOrControl+',
  ])('rejects an unsupported hotkey: %s', (input) => {
    expect(() => normalizePlatformHotkey(input)).toThrow('invalid_hotkey');
  });

  it('uses one cross-platform ASCII lowercase rule for executable names', () => {
    expect(normalizeExecutableDenylistEntry('  Editor.EXE  ')).toBe('editor.exe');
    expect(normalizeExecutableDenylistEntry('Clipboard_Helper-2')).toBe(
      'clipboard_helper-2',
    );
    expect(() => normalizeExecutableDenylistEntry('AplikacjaŻ')).toThrow(
      'invalid_denylist_entry',
    );
  });

  it('canonicalizes bundle ids and deduplicates only after normalization', () => {
    expect(
      normalizeDenylistEntries([
        ' COM.Acme.Editor ',
        'com.acme.editor',
        'Editor.EXE',
        'editor.exe',
        '',
      ]),
    ).toEqual(['com.acme.editor', 'editor.exe']);
  });

  it.each([
    ['/Applications/Editor.app', 'path separator'],
    ['folder\\editor.exe', 'path separator'],
    ['editor\u0000.exe', 'control character'],
    [`${'a'.repeat(257)}.exe`, 'UTF-8 byte cap'],
  ])('rejects an invalid denylist entry with a %s', (entry) => {
    expect(() => normalizeDenylistEntries([entry])).toThrow('invalid_denylist_entry');
  });

  it('caps the normalized denylist at 200 entries', () => {
    expect(() =>
      normalizeDenylistEntries(
        Array.from({ length: 201 }, (_, index) => `editor-${index}.exe`),
      ),
    ).toThrow('denylist_too_large');
  });
});

describe('SettingsPanel validation and transactions', () => {
  it.each(['0', '3651', '1.5'])('rejects retention outside integer 1 through 3650: %s', async (days) => {
    const saveSettings = vi.fn<ClipboardGateway['saveSettings']>();
    const user = userEvent.setup();
    render(<SettingsPanel gateway={makeGateway({ saveSettings })} />);
    await loadSettings();

    const retention = screen.getByRole('spinbutton', { name: 'Dni przechowywania' });
    await user.clear(retention);
    await user.type(retention, days);
    await user.click(screen.getByRole('button', { name: 'Zapisz ustawienia' }));

    expect(await screen.findByText('Podaj pełną liczbę dni od 1 do 3650.')).toBeVisible();
    expect(saveSettings).not.toHaveBeenCalled();
  });

  it.each(['1', '3650'])('accepts the inclusive retention boundary: %s', async (days) => {
    const saveSettings = vi.fn(async (nextSettings: AppSettings) => nextSettings);
    const user = userEvent.setup();
    render(<SettingsPanel gateway={makeGateway({ saveSettings })} />);
    await loadSettings();

    const retention = screen.getByRole('spinbutton', { name: 'Dni przechowywania' });
    await user.clear(retention);
    await user.type(retention, days);
    await user.click(screen.getByRole('button', { name: 'Zapisz ustawienia' }));

    await waitFor(() => expect(saveSettings).toHaveBeenCalled());
    expect(saveSettings.mock.calls[0]?.[0].retentionDays).toBe(Number(days));
  });

  it('stores unlimited retention as null', async () => {
    const saveSettings = vi.fn(async (nextSettings: AppSettings) => nextSettings);
    const user = userEvent.setup();
    render(<SettingsPanel gateway={makeGateway({ saveSettings })} />);
    await loadSettings();

    await user.click(screen.getByRole('checkbox', { name: 'Bez limitu retencji' }));
    expect(screen.getByRole('spinbutton', { name: 'Dni przechowywania' })).toBeDisabled();
    await user.click(screen.getByRole('button', { name: 'Zapisz ustawienia' }));

    await waitFor(() => expect(saveSettings).toHaveBeenCalled());
    expect(saveSettings.mock.calls[0]?.[0].retentionDays).toBeNull();
  });

  it('applies native autostart before persisting settings', async () => {
    const order: string[] = [];
    const setAutostartEnabled = vi.fn(async (enabled: boolean) => {
      order.push(`native:${enabled}`);
    });
    const saveSettings = vi.fn(async (nextSettings: AppSettings) => {
      order.push('persisted');
      return nextSettings;
    });
    const isAutostartEnabled = vi
      .fn<ClipboardGateway['isAutostartEnabled']>()
      .mockResolvedValueOnce(false)
      .mockResolvedValueOnce(true);
    const user = userEvent.setup();
    render(
      <SettingsPanel
        gateway={makeGateway({ isAutostartEnabled, saveSettings, setAutostartEnabled })}
      />,
    );
    await loadSettings();

    await user.click(screen.getByRole('checkbox', { name: 'Uruchamiaj przy logowaniu' }));
    await user.click(screen.getByRole('button', { name: 'Zapisz ustawienia' }));

    expect(await screen.findByRole('status')).toHaveTextContent('Ustawienia zapisane');
    expect(order).toEqual(['native:true', 'persisted']);
    expect(saveSettings.mock.calls[0]?.[0].autostart).toBe(true);
  });

  it('does not persist when the native autostart operation fails', async () => {
    const saveSettings = vi.fn<ClipboardGateway['saveSettings']>();
    const isAutostartEnabled = vi.fn(async () => false);
    const setAutostartEnabled = vi.fn(async () => {
      throw new Error('/private/native/plugin detail');
    });
    const user = userEvent.setup();
    const { container } = render(
      <SettingsPanel
        gateway={makeGateway({ isAutostartEnabled, saveSettings, setAutostartEnabled })}
      />,
    );
    await loadSettings();

    await user.click(screen.getByRole('checkbox', { name: 'Uruchamiaj przy logowaniu' }));
    await user.click(screen.getByRole('button', { name: 'Zapisz ustawienia' }));

    expect(await screen.findByRole('alert')).toHaveTextContent(
      'Nie udało się zastosować ustawień',
    );
    expect(saveSettings).not.toHaveBeenCalled();
    expect(isAutostartEnabled).toHaveBeenCalledTimes(2);
    expect(container).not.toHaveTextContent('/private/native');
  });

  it('rolls native autostart back and re-queries it when persistence fails', async () => {
    const setAutostartEnabled = vi.fn(async () => undefined);
    const isAutostartEnabled = vi
      .fn<ClipboardGateway['isAutostartEnabled']>()
      .mockResolvedValueOnce(false)
      .mockResolvedValueOnce(false);
    const saveSettings = vi.fn(async () => {
      throw new Error('/private/settings.json database detail');
    });
    const user = userEvent.setup();
    const { container } = render(
      <SettingsPanel
        gateway={makeGateway({ isAutostartEnabled, saveSettings, setAutostartEnabled })}
      />,
    );
    await loadSettings();

    await user.click(screen.getByRole('checkbox', { name: 'Uruchamiaj przy logowaniu' }));
    await user.click(screen.getByRole('button', { name: 'Zapisz ustawienia' }));

    expect(await screen.findByRole('alert')).toHaveTextContent(
      'Nie udało się zastosować ustawień',
    );
    expect(setAutostartEnabled.mock.calls).toEqual([[true], [false]]);
    expect(isAutostartEnabled).toHaveBeenCalledTimes(2);
    expect(screen.getByRole('checkbox', { name: 'Uruchamiaj przy logowaniu' })).toBeChecked();
    expect(container).not.toHaveTextContent('/private/settings.json');
  });

  it('disables repeated submits while a transaction is pending', async () => {
    const save = deferred<AppSettings>();
    const saveSettings = vi.fn(() => save.promise);
    const user = userEvent.setup();
    render(<SettingsPanel gateway={makeGateway({ saveSettings })} />);
    await loadSettings();

    const submit = screen.getByRole('button', { name: 'Zapisz ustawienia' });
    await user.click(submit);
    expect(screen.getByRole('button', { name: 'Zapisywanie…' })).toBeDisabled();
    fireEvent.click(screen.getByRole('button', { name: 'Zapisywanie…' }));
    expect(saveSettings).toHaveBeenCalledOnce();

    await act(async () => save.resolve(persistedSettings));
  });

  it('shows a persisted/native autostart mismatch explicitly', async () => {
    render(
      <SettingsPanel
        gateway={makeGateway({
          getSettings: vi.fn(async () => ({ ...persistedSettings, autostart: true })),
          isAutostartEnabled: vi.fn(async () => false),
        })}
      />,
    );
    await loadSettings();

    expect(screen.getByRole('status')).toHaveTextContent(
      'Stan autostartu różni się od zapisanego ustawienia',
    );
    expect(screen.getByText(/zapisane: włączony/i)).toBeVisible();
    expect(screen.getByText(/system: wyłączony/i)).toBeVisible();
  });
});

describe('Settings dialog and storage semantics', () => {
  it('labels the dialog, explains UI-only hotkey behavior, and traps initial focus', async () => {
    const user = userEvent.setup();
    render(<SettingsPanel gateway={makeGateway()} />);
    const hotkey = await loadSettings();

    const dialog = screen.getByRole('dialog', { name: 'Ustawienia' });
    expect(dialog).toHaveAttribute('aria-modal', 'true');
    expect(dialog).toHaveAccessibleDescription(/ustawienia pozostają lokalne/i);
    // Saving now rebinds the shortcut with the system, so the screen must say
    // that rather than promising it takes effect on the next launch.
    expect(screen.getByText(/zapisanie zmienia aktywny skrót od razu/i)).toBeVisible();
    expect(hotkey).toHaveFocus();

    const close = screen.getByRole('button', { name: 'Zamknij ustawienia' });
    close.focus();
    await user.tab();
    expect(hotkey).toHaveFocus();
    await user.tab({ shift: true });
    expect(close).toHaveFocus();
  });

  it('describes the database main file and referenced blobs without claiming total disk use', () => {
    render(<StorageStatsView stats={storageStats} status="ready" />);

    expect(screen.getByText('Główny plik bazy danych')).toBeVisible();
    expect(screen.getByText('Bloby wskazane przez bazę')).toBeVisible();
    expect(screen.getByText(/nie jest to całkowite użycie dysku/i)).toBeVisible();
    expect(screen.queryByText('Łączny rozmiar aplikacji')).not.toBeInTheDocument();
  });

  it('shows unavailable instead of rendering unsafe storage values', async () => {
    render(
      <SettingsPanel
        gateway={makeGateway({
          getStorageStats: vi.fn(async () => ({
            ...storageStats,
            databaseBytes: Number.MAX_SAFE_INTEGER,
            blobBytes: 1,
          })),
        })}
      />,
    );
    await loadSettings();

    expect(await screen.findByText('Dane o pamięci są niedostępne.')).toBeVisible();
    expect(screen.queryByText(`${Number.MAX_SAFE_INTEGER} B`)).not.toBeInTheDocument();
  });
});
