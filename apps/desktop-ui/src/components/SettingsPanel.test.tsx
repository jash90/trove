import '@testing-library/jest-dom/vitest';
import { act, fireEvent, render, screen, waitFor, within } from '@testing-library/react';
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
  linkPreviews: true,
  keyvault: { url: null, token: null, privateJwk: null },
};

const vaultSecrets = [
  { slug: 'openai', name: 'OpenAI', category: 'ai' as const },
  { slug: 'github', name: 'GitHub', category: null },
];

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
    keyvaultList: vi.fn(async () => vaultSecrets.map((secret) => ({ ...secret }))),
    keyvaultCopySecret: vi.fn(async () => undefined),
    keyvaultPairStart: vi.fn(async () => 'A1B2-C3D4'),
    keyvaultPairPoll: vi.fn(async () => ({ status: 'paired' as const })),
    keyvaultPairCancel: vi.fn(async () => undefined),
    ...overrides,
  }) as ClipboardGateway;

const loadSettings = async (): Promise<HTMLInputElement> =>
  screen.findByRole('textbox', { name: 'Global shortcut' });

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
    expect(() => normalizeExecutableDenylistEntry('Applicationé')).toThrow(
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
    await user.click(screen.getByRole('button', { name: 'Save settings' }));

    expect(await screen.findByText('Give a whole number of days from 1 to 3650.')).toBeVisible();
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
    await user.click(screen.getByRole('button', { name: 'Save settings' }));

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
    await user.click(screen.getByRole('button', { name: 'Save settings' }));

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
    await user.click(screen.getByRole('button', { name: 'Save settings' }));

    expect(await screen.findByRole('status')).toHaveTextContent('Settings saved');
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
    await user.click(screen.getByRole('button', { name: 'Save settings' }));

    expect(await screen.findByRole('alert')).toHaveTextContent(
      'The settings could not be applied',
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
    await user.click(screen.getByRole('button', { name: 'Save settings' }));

    expect(await screen.findByRole('alert')).toHaveTextContent(
      'The settings could not be applied',
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

    const submit = screen.getByRole('button', { name: 'Save settings' });
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
      'The autostart state differs from the saved setting',
    );
    expect(screen.getByText(/saved: on/i)).toBeVisible();
    expect(screen.getByText(/system: off/i)).toBeVisible();
  });
});

describe('Settings page and storage semantics', () => {
  it('labels the page, explains hotkey behavior, and starts on the first field', async () => {
    render(<SettingsPanel gateway={makeGateway()} />);
    const hotkey = await loadSettings();

    // Its own window now, so there is nothing behind it to trap focus against
    // and nothing to mark as modal.
    const page = screen.getByRole('main', { name: 'Settings' });
    expect(page).not.toHaveAttribute('aria-modal');
    expect(page).toHaveAccessibleDescription(/settings stay local/i);
    // Saving rebinds the shortcut with the system, so the screen must say that
    // rather than promising it takes effect on the next launch.
    expect(screen.getByText(/saving changes the active shortcut immediately/i)).toBeVisible();
    expect(hotkey).toHaveFocus();
    expect(screen.getByRole('button', { name: 'Close settings' })).toBeVisible();
  });

  it('describes the database main file and referenced blobs without claiming total disk use', () => {
    render(<StorageStatsView stats={storageStats} status="ready" />);

    expect(screen.getByText('Main database file')).toBeVisible();
    expect(screen.getByText('Blobs the database references')).toBeVisible();
    expect(screen.getByText(/not the application's total disk use/i)).toBeVisible();
    expect(screen.queryByText('Total application size')).not.toBeInTheDocument();
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

    expect(await screen.findByText('Storage figures are unavailable.')).toBeVisible();
    expect(screen.queryByText(`${Number.MAX_SAFE_INTEGER} B`)).not.toBeInTheDocument();
  });
});

describe('keyvault section', () => {
  it('saves the overrides and never asks for a private key', async () => {
    const gateway = makeGateway();
    render(<SettingsPanel gateway={gateway} />);
    await loadSettings();

    // The key belongs to the device identity file. There is deliberately no
    // field for it here, and this is the assertion that keeps one from coming
    // back: a second copy of the key is what the identity file exists to stop.
    expect(screen.queryByLabelText(/Private key/u)).not.toBeInTheDocument();

    await userEvent.type(screen.getByLabelText(/Vault address/u), 'https://vault.example.invalid');
    await userEvent.type(screen.getByLabelText(/Agent token/u), 'kv_synthetic-token');
    fireEvent.submit(screen.getByRole('button', { name: 'Save settings' }).closest('form')!);

    await waitFor(() => {
      expect(gateway.saveSettings).toHaveBeenCalledWith(
        expect.objectContaining({
          keyvault: {
            url: 'https://vault.example.invalid',
            token: 'kv_synthetic-token',
          },
        }),
      );
    });
  });

  it('leaves both overrides absent when the fields are untouched', async () => {
    const gateway = makeGateway();
    render(<SettingsPanel gateway={gateway} />);
    await loadSettings();

    fireEvent.submit(screen.getByRole('button', { name: 'Save settings' }).closest('form')!);

    // Blank means "use the device identity", which travels as null rather than
    // as an empty string the core would have to special-case.
    await waitFor(() => {
      expect(gateway.saveSettings).toHaveBeenCalledWith(
        expect.objectContaining({ keyvault: { url: null, token: null } }),
      );
    });
  });

  it('clears the fields once pairing has replaced what was in them', async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    try {
      await clearsFieldsAfterPairing();
    } finally {
      vi.useRealTimers();
    }
  });

  const clearsFieldsAfterPairing = async (): Promise<void> => {
    const gateway = makeGateway();
    // The core clears the overrides as part of pairing, so the settings it hands back afterwards
    // no longer carry them.
    gateway.getSettings = vi.fn(async () => ({
      ...persistedSettings,
      keyvault: { url: null, token: null },
    })) as typeof gateway.getSettings;
    render(<SettingsPanel gateway={gateway} />);
    await loadSettings();

    await userEvent.type(screen.getByLabelText(/Vault address/u), 'https://old.example.invalid');
    expect(await screen.findByText(/override the paired device identity/u)).toBeVisible();

    await userEvent.click(screen.getByRole('button', { name: 'Connect' }));

    // The poll reports success; the pane must stop showing text the database no longer holds,
    // or a later Save writes it back and breaks the pairing for real.
    // Driven rather than waited out. The pane polls every two seconds, and a test that sits
    // through that on the wall clock fails whenever the suite runs the file under load — which
    // it did. Advancing the timers asserts what the poll does without depending on how long the
    // machine takes to get there.
    await vi.advanceTimersByTimeAsync(2500);
    await waitFor(() => {
      expect(screen.getByLabelText(/Vault address/u)).toHaveValue('');
    });
    expect(screen.queryByText(/override the paired device identity/u)).not.toBeInTheDocument();
  };

  it('says plainly when a leftover override is what the vault is being asked with', async () => {
    const gateway = makeGateway();
    render(<SettingsPanel gateway={gateway} />);
    await loadSettings();

    // Nothing typed: the pairing is in charge and there is nothing to warn about.
    expect(screen.queryByText(/override the paired device identity/u)).not.toBeInTheDocument();

    // A token left over from an earlier configuration silently outranks a working pairing and
    // the vault answers 401. The pane used to show a filled field and a refusal without ever
    // connecting the two, which is exactly how that went unnoticed.
    await userEvent.type(screen.getByLabelText(/Agent token/u), 'kv_stale0123456789');

    expect(await screen.findByText(/override the paired device identity/u)).toBeVisible();
  });

  it('connects with an empty address, because after the first pairing there is none to type', async () => {
    const gateway = makeGateway();
    render(<SettingsPanel gateway={gateway} />);
    await loadSettings();

    // Pairing clears this field on success, so blank is the ordinary state of a paired install.
    // Refusing here is what made a second pairing impossible: the act of pairing removed the only
    // thing the next one could have read. The core resolves the address from the device identity.
    expect(screen.getByLabelText(/Vault address/u)).toHaveValue('');
    await userEvent.click(screen.getByRole('button', { name: 'Connect' }));

    await waitFor(() => {
      expect(gateway.keyvaultPairStart).toHaveBeenCalledWith('');
    });
  });

  it('explains itself when nothing on the device names a vault', async () => {
    const gateway = makeGateway();
    gateway.keyvaultPairStart = vi.fn(async () => {
      throw new Error('keyvault_no_vault_address');
    }) as typeof gateway.keyvaultPairStart;
    render(<SettingsPanel gateway={gateway} />);
    await loadSettings();

    await userEvent.click(screen.getByRole('button', { name: 'Connect' }));

    // Distinct from a malformed address on purpose: this one is fixed by typing something, and
    // saying so beats a generic refusal.
    expect(await screen.findByText(/Nothing here names a vault yet/u)).toBeVisible();
  });

  it('shows the fingerprint while waiting, because approving without it proves nothing', async () => {
    const gateway = makeGateway();
    render(<SettingsPanel gateway={gateway} />);
    await loadSettings();

    await userEvent.type(screen.getByLabelText(/Vault address/u), 'https://vault.example.invalid');
    await userEvent.click(screen.getByRole('button', { name: 'Connect' }));

    await waitFor(() => {
      expect(gateway.keyvaultPairStart).toHaveBeenCalledWith('https://vault.example.invalid');
    });

    // The fingerprint is the whole point of the waiting state: it is what the person compares
    // against the browser before approving.
    expect(await screen.findByText('A1B2-C3D4')).toBeVisible();
    expect(screen.getByRole('button', { name: 'Waiting for approval…' })).toBeDisabled();
  });

  it('tests the connection by listing metadata, and copies without showing a value', async () => {
    const gateway = makeGateway();
    render(<SettingsPanel gateway={gateway} />);
    await loadSettings();

    await userEvent.click(screen.getByRole('button', { name: 'Test connection' }));

    const list = await screen.findByRole('list', { name: 'Vault secrets' });
    expect(within(list).getAllByRole('listitem')).toHaveLength(2);

    await userEvent.click(within(list).getByRole('button', { name: 'Copy openai' }));

    await waitFor(() => {
      expect(gateway.keyvaultCopySecret).toHaveBeenCalledWith('openai');
    });
    expect(
      screen.getByText('openai is on the clipboard. Paste it where it is needed.'),
    ).toBeVisible();
  });

  it('surfaces a vault denial as a plain sentence', async () => {
    // The core rejects with the bare code string, not an Error — the mock
    // matches that shape so the mapper is exercised the way production runs.
    const gateway = makeGateway({
      keyvaultList: vi.fn(async () => {
        throw 'keyvault_unauthorized';
      }),
    });
    render(<SettingsPanel gateway={gateway} />);
    await loadSettings();

    await userEvent.click(screen.getByRole('button', { name: 'Test connection' }));

    expect(
      await screen.findByText('The token was refused — create a new one in the vault.'),
    ).toBeVisible();
  });
});
