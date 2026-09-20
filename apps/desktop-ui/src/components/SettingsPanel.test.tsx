import '@testing-library/jest-dom/vitest';
import { act, fireEvent, render, screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { describe, expect, it, vi } from 'vitest';

import type { AppSettings, ShortcutStatus, StorageStats } from '../lib/contracts';
import type { ClipboardGateway } from '../lib/gateway';
import {
  acceleratorFromKeyEvent,
  expiryLabel,
  normalizeDenylistEntries,
  normalizeExecutableDenylistEntry,
  normalizePlatformHotkey,
  SettingsPanel,
  shortcutReleaseNotice,
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
  hotkey: 'CommandOrControl+Space',
  autostart: false,
  paletteModes: true,
  dockIcon: false,
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
    keyvaultPairStart: vi.fn(async () => ({
      fingerprint: 'A1B2-C3D4',
      url: 'https://vault.example.invalid/pair?code=synthetic-code',
      code: 'synthetic-code',
      expiresAt: Date.now() + 30 * 60 * 1000,
    })),
    keyvaultPairPoll: vi.fn(async () => ({ status: 'paired' as const })),
    keyvaultPairCancel: vi.fn(async () => undefined),
    keyvaultIdentity: vi.fn(async () => ({ paired: false, url: null })),
    keyvaultResetPairing: vi.fn(async () => undefined),
    ...overrides,
  }) as ClipboardGateway;

const loadSettings = async (): Promise<HTMLInputElement> =>
  screen.findByRole('textbox', { name: 'Global shortcut' });

/// Brings a settings tab into view.
///
/// Only the selected tab is rendered, so a test that reaches for a field on another one finds
/// nothing. That is the shape of the component now, and asking for the tab first is what a person
/// does too.
const openTab = async (name: string): Promise<void> => {
  await userEvent.click(screen.getByRole('tab', { name }));
};

const heldBySystem: ShortcutStatus = {
  hotkey: 'CommandOrControl+Space',
  registered: true,
  heldBySystem: true,
  releasedIds: [],
};

describe('the system shortcut standing on ours', () => {
  it('offers to free the chord, and reports what the system actually did', async () => {
    // Registration succeeding tells the user nothing: macOS answers ⌘Space
    // above the table this application registers into, so the shortcut binds
    // cleanly and then never fires.
    const freeSummoningShortcut = vi.fn<NonNullable<ClipboardGateway['freeSummoningShortcut']>>(
      async () => 'applied' as const,
    );
    const getShortcutStatus = vi
      .fn<NonNullable<ClipboardGateway['getShortcutStatus']>>()
      .mockResolvedValueOnce(heldBySystem)
      .mockResolvedValue({ ...heldBySystem, heldBySystem: false, releasedIds: [64] });
    const user = userEvent.setup();
    render(<SettingsPanel gateway={makeGateway({ getShortcutStatus, freeSummoningShortcut })} />);
    await loadSettings();

    await user.click(await screen.findByRole('button', { name: 'Free it for Trove' }));
    await waitFor(() => expect(freeSummoningShortcut).toHaveBeenCalled());

    // The system is read back rather than assumed, so what is offered next is
    // the undo — not the same button again.
    expect(
      await screen.findByRole('button', { name: 'Give it back to the system' }),
    ).toBeInTheDocument();
  });

  it('says nothing when the chord is already free', async () => {
    const getShortcutStatus = vi.fn(async () => ({ ...heldBySystem, heldBySystem: false }));
    render(<SettingsPanel gateway={makeGateway({ getShortcutStatus })} />);
    await loadSettings();

    expect(screen.queryByRole('button', { name: 'Free it for Trove' })).not.toBeInTheDocument();
  });

  it('names the other application when the binding itself was refused', async () => {
    // A different failure with a different fix, and it looks identical from the
    // keyboard: nothing happens when the key is pressed.
    const getShortcutStatus = vi.fn(async () => ({
      ...heldBySystem,
      registered: false,
      heldBySystem: false,
    }));
    render(<SettingsPanel gateway={makeGateway({ getShortcutStatus })} />);
    await loadSettings();

    expect(await screen.findByText(/Another application is holding/u)).toBeInTheDocument();
  });

  it.each([
    ['applied', null],
    ['alreadyFree', 'Nothing in the system was holding that shortcut.'],
    ['needsLogout', 'Saved. It takes effect after you log out and back in.'],
  ] as const)('reports %s honestly rather than as a plain success', (outcome, expected) => {
    // `needsLogout` is the one worth spelling out: the preference is written
    // and will hold, but the running session has not picked it up, so calling
    // it done would send the user off to press a key that still opens
    // Spotlight.
    expect(shortcutReleaseNotice(outcome)).toBe(expected);
  });

  it('falls back to the manual route when the system refuses the change', () => {
    expect(shortcutReleaseNotice('refused')).toMatch(/Keyboard Shortcuts/u);
  });
});

describe('settings normalization', () => {
  it.each([
    // One modifier is enough, and it has to be: ⌘Space is the shortcut this
    // application ships with.
    ['commandorcontrol + space', 'CommandOrControl+Space'],
    ['commandorcontrol + shift + space', 'CommandOrControl+Shift+Space'],
    ['commandorcontrol + shift + v', 'CommandOrControl+Shift+V'],
    ['control+alt+7', 'Control+Alt+7'],
    ['Command+Shift+F12', 'Command+Shift+F12'],
    ['alt + space', 'Alt+Space'],
    ['Alt+Shift+K', 'Alt+Shift+K'],
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
    // Shift is not a modifier a global binding can stand on: this is how a capital V is typed.
    'Shift+V',
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
    await openTab('Retention');

    const retention = screen.getByRole('spinbutton', { name: 'Days kept' });
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
    await openTab('Retention');

    const retention = screen.getByRole('spinbutton', { name: 'Days kept' });
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
    await openTab('Retention');

    await user.click(screen.getByRole('checkbox', { name: 'Bez limitu retencji' }));
    expect(screen.getByRole('spinbutton', { name: 'Days kept' })).toBeDisabled();
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

    await user.click(screen.getByRole('checkbox', { name: 'Launch at login' }));
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

    await user.click(screen.getByRole('checkbox', { name: 'Launch at login' }));
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

    await user.click(screen.getByRole('checkbox', { name: 'Launch at login' }));
    await user.click(screen.getByRole('button', { name: 'Save settings' }));

    expect(await screen.findByRole('alert')).toHaveTextContent(
      'The settings could not be applied',
    );
    expect(setAutostartEnabled.mock.calls).toEqual([[true], [false]]);
    expect(isAutostartEnabled).toHaveBeenCalledTimes(2);
    expect(screen.getByRole('checkbox', { name: 'Launch at login' })).toBeChecked();
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
    expect(screen.getByRole('button', { name: 'Saving…' })).toBeDisabled();
    fireEvent.click(screen.getByRole('button', { name: 'Saving…' }));
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

  it('saves the Dock tile the user ticked', async () => {
    // The tile is a property of the running process, so the only evidence
    // this pane can give is that the value reached the core. What the core
    // does with it is covered on the Rust side.
    const saveSettings = vi.fn(async (nextSettings: AppSettings) => nextSettings);
    const user = userEvent.setup();
    render(<SettingsPanel gateway={makeGateway({ saveSettings })} />);
    await loadSettings();

    expect(screen.getByRole('checkbox', { name: 'Show in the Dock' })).not.toBeChecked();
    await user.click(screen.getByRole('checkbox', { name: 'Show in the Dock' }));
    await user.click(screen.getByRole('button', { name: 'Save settings' }));

    await waitFor(() => expect(saveSettings).toHaveBeenCalled());
    expect(saveSettings.mock.calls[0]?.[0].dockIcon).toBe(true);
  });

  it('shows the Dock tile the settings row remembers', async () => {
    const user = userEvent.setup();
    render(
      <SettingsPanel
        gateway={makeGateway({
          getSettings: vi.fn(async () => ({ ...persistedSettings, dockIcon: true })),
        })}
      />,
    );
    await loadSettings();

    expect(screen.getByRole('checkbox', { name: 'Show in the Dock' })).toBeChecked();
    // Untouched, it goes back exactly as it came.
    await user.click(screen.getByRole('button', { name: 'Save settings' }));
    await waitFor(() => expect(screen.getByRole('status')).toHaveTextContent('Settings saved'));
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
    await openTab('Storage');

    expect(await screen.findByText('Storage figures are unavailable.')).toBeVisible();
    expect(screen.queryByText(`${Number.MAX_SAFE_INTEGER} B`)).not.toBeInTheDocument();
  });
});

describe('recording a shortcut', () => {
  const press = (code: string, mods: Partial<Record<'metaKey' | 'ctrlKey' | 'altKey' | 'shiftKey', boolean>> = {}) =>
    acceleratorFromKeyEvent({
      code,
      metaKey: false,
      ctrlKey: false,
      altKey: false,
      shiftKey: false,
      ...mods,
    });

  it('turns a pressed combination into the accelerator the core stores', () => {
    expect(press('KeyV', { metaKey: true, shiftKey: true })).toBe('CommandOrControl+Shift+V');
    expect(press('Space', { metaKey: true })).toBe('CommandOrControl+Space');
    expect(press('Digit1', { ctrlKey: true, altKey: true })).toBe('Control+Alt+1');
    expect(press('F5', { metaKey: true })).toBe('CommandOrControl+F5');
  });

  it('accepts Alt as a modifier in its own right', () => {
    // ⌥Space is the shortcut people arrive expecting, so Alt has to stand without ⌘ or ⌃.
    expect(press('Space', { altKey: true })).toBe('Alt+Space');
    expect(press('KeyK', { altKey: true, shiftKey: true })).toBe('Alt+Shift+K');
  });

  it('records nothing when no modifier, or only Shift, is held', () => {
    // A bare key or Shift+key is ordinary typing — binding one globally would swallow it in every
    // other application, so pressing it must leave the setting alone.
    expect(press('KeyV')).toBeNull();
    expect(press('KeyV', { shiftKey: true })).toBeNull();
  });

  it('records nothing from modifiers alone or from a key it cannot name', () => {
    // Holding modifiers is a combination in progress, not a combination.
    expect(press('MetaLeft', { metaKey: true })).toBeNull();
    expect(press('ShiftLeft', { shiftKey: true })).toBeNull();
    expect(press('Enter', { metaKey: true })).toBeNull();

    // Command and Control together would be two primaries, which is not one physical key.
    expect(press('KeyV', { metaKey: true, ctrlKey: true })).toBeNull();
  });

  it('shows the recorded shortcut and refuses typed text', async () => {
    const saveSettings = vi.fn<ClipboardGateway['saveSettings']>(async (settings) => settings);
    const user = userEvent.setup();
    render(<SettingsPanel gateway={makeGateway({ saveSettings })} />);
    const field = await loadSettings();

    // Typing used to be the only way to set this, and it meant knowing the accelerator syntax.
    await user.type(field, 'nonsense');
    expect(field).toHaveValue('CommandOrControl+Space');

    fireEvent.keyDown(field, { code: 'KeyK', metaKey: true, altKey: true });
    expect(field).toHaveValue('CommandOrControl+Alt+K');

    await user.click(screen.getByRole('button', { name: 'Save settings' }));
    await waitFor(() => {
      expect(saveSettings).toHaveBeenCalledWith(
        expect.objectContaining({ hotkey: 'CommandOrControl+Alt+K' }),
      );
    });
  });
});

describe('settings tabs', () => {
  it('shows one section at a time and names the selected tab', async () => {
    render(<SettingsPanel gateway={makeGateway()} />);
    await loadSettings();

    const tabs = screen.getAllByRole('tab');
    expect(tabs).toHaveLength(8);
    expect(screen.getByRole('tab', { name: 'Shortcut' })).toHaveAttribute('aria-selected', 'true');

    // The point of tabs: the other five sections are not on screen competing for the eye.
    expect(screen.getByRole('textbox', { name: 'Global shortcut' })).toBeVisible();
    expect(screen.queryByRole('spinbutton', { name: 'Days kept' })).not.toBeInTheDocument();

    await openTab('Retention');
    expect(screen.getByRole('spinbutton', { name: 'Days kept' })).toBeVisible();
    expect(screen.queryByRole('textbox', { name: 'Global shortcut' })).not.toBeInTheDocument();
  });

  it('keeps the storage figures on their own tab and nowhere else', async () => {
    render(<SettingsPanel gateway={makeGateway()} />);
    await loadSettings();

    // It used to render on every tab: the tab wrapping went around the six <section> elements
    // and this one is a sibling component, so it slipped through. Asserting its absence
    // elsewhere is the assertion that catches that; asserting its presence on its own tab would
    // have passed the whole time it was wrong.
    expect(screen.queryByRole('heading', { name: 'Data storage' })).not.toBeInTheDocument();

    await openTab('Retention');
    expect(screen.queryByRole('heading', { name: 'Data storage' })).not.toBeInTheDocument();

    await openTab('Storage');
    expect(await screen.findByRole('heading', { name: 'Data storage' })).toBeVisible();
  });

  it('moves between tabs with the arrows, so the strip is one stop and not six', async () => {
    const user = userEvent.setup();
    render(<SettingsPanel gateway={makeGateway()} />);
    await loadSettings();

    const shortcut = screen.getByRole('tab', { name: 'Shortcut' });
    shortcut.focus();
    await user.keyboard('{ArrowRight}');

    const retention = screen.getByRole('tab', { name: 'Retention' });
    expect(retention).toHaveAttribute('aria-selected', 'true');
    // Focus follows the selection, or the next arrow press would start from somewhere else.
    expect(retention).toHaveFocus();

    await user.keyboard('{End}');
    expect(screen.getByRole('tab', { name: 'Export' })).toHaveAttribute('aria-selected', 'true');
  });

  it('keeps an edit made on one tab when another is opened and saved', async () => {
    const saveSettings = vi.fn<ClipboardGateway['saveSettings']>(async (settings) => settings);
    const user = userEvent.setup();
    render(<SettingsPanel gateway={makeGateway({ saveSettings })} />);
    await loadSettings();

    // Hidden tabs are unmounted, so their inputs are gone from the document. The values live in
    // the panel's own state — and this is the assertion that keeps them there, because losing an
    // edit on tab switch is the quiet way tabs go wrong.
    await openTab('Retention');
    const retention = screen.getByRole('spinbutton', { name: 'Days kept' });
    await user.clear(retention);
    await user.type(retention, '90');

    await openTab('Shortcut');
    await user.click(screen.getByRole('button', { name: 'Save settings' }));

    await waitFor(() => {
      expect(saveSettings).toHaveBeenCalledWith(expect.objectContaining({ retentionDays: 90 }));
    });
  });
});

describe('keyvault section', () => {
  it('remembers the address and never asks for a token or a key', async () => {
    const gateway = makeGateway();
    render(<SettingsPanel gateway={gateway} />);
    await loadSettings();
    await openTab('Keyvault');

    // Neither belongs to this pane. The key was never ours; the token comes from pairing, and a
    // pasted one only ever shadowed it — which is how a successful pairing reported a refusal.
    expect(screen.queryByLabelText(/Private key/u)).not.toBeInTheDocument();
    expect(screen.queryByLabelText(/Agent token/u)).not.toBeInTheDocument();

    await userEvent.type(screen.getByLabelText(/Vault address/u), 'https://vault.example.invalid');
    fireEvent.submit(screen.getByRole('button', { name: 'Save settings' }).closest('form')!);

    await waitFor(() => {
      expect(gateway.saveSettings).toHaveBeenCalledWith(
        expect.objectContaining({
          keyvault: { url: 'https://vault.example.invalid', token: null },
        }),
      );
    });
  });

  it('sends no address when the field is untouched', async () => {
    const gateway = makeGateway();
    render(<SettingsPanel gateway={gateway} />);
    await loadSettings();
    await openTab('Keyvault');

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
    await openTab('Keyvault');

    await userEvent.type(screen.getByLabelText(/Vault address/u), 'https://old.example.invalid');

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
  };

  it('locks the address once paired and offers a reset instead', async () => {
    const gateway = makeGateway();
    gateway.keyvaultIdentity = vi.fn(async () => ({
      paired: true,
      url: 'https://trustworthy-eagle-783.convex.site',
    })) as typeof gateway.keyvaultIdentity;
    render(<SettingsPanel gateway={gateway} />);
    await loadSettings();
    await openTab('Keyvault');

    // Shown, so it is obvious which vault this is, and locked, because editing it would change
    // nothing: reads go to the pairing. A field that looks editable and is ignored is the trap
    // this pane used to be.
    const address = await screen.findByLabelText(/Vault address/u);
    await waitFor(() => expect(address).toBeDisabled());
    expect(address).toHaveValue('https://trustworthy-eagle-783.convex.site');
    expect(screen.getByRole('button', { name: 'Connect' })).toBeDisabled();

    await userEvent.click(screen.getByRole('button', { name: 'Reset' }));

    await waitFor(() => expect(gateway.keyvaultResetPairing).toHaveBeenCalledOnce());
  });

  it('connects with an empty address, because after the first pairing there is none to type', async () => {
    const gateway = makeGateway();
    render(<SettingsPanel gateway={gateway} />);
    await loadSettings();
    await openTab('Keyvault');

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
    await openTab('Keyvault');

    await userEvent.click(screen.getByRole('button', { name: 'Connect' }));

    // Distinct from a malformed address on purpose: this one is fixed by typing something, and
    // saying so beats a generic refusal.
    expect(await screen.findByText(/Nothing here names a vault yet/u)).toBeVisible();
  });

  it('shows a copyable link and the bare code, because the browser that opens may be the wrong one', async () => {
    const writeText = vi.fn(async () => undefined);
    // Restored below: left in place it would follow every later test in this file, and a global
    // this test installed is not a fact about the others.
    const original = Object.getOwnPropertyDescriptor(navigator, 'clipboard');
    Object.defineProperty(navigator, 'clipboard', { value: { writeText }, configurable: true });
    try {
      await copyableLinkIsShown(writeText);
    } finally {
      if (original) Object.defineProperty(navigator, 'clipboard', original);
      else Reflect.deleteProperty(navigator as unknown as Record<string, unknown>, 'clipboard');
    }
  });

  const copyableLinkIsShown = async (writeText: ReturnType<typeof vi.fn>): Promise<void> => {
    const gateway = makeGateway();
    render(<SettingsPanel gateway={gateway} />);
    await loadSettings();
    await openTab('Keyvault');

    await userEvent.type(screen.getByLabelText(/Vault address/u), 'https://vault.example.invalid');
    await userEvent.click(screen.getByRole('button', { name: 'Connect' }));

    // The app opens the default browser, which is not necessarily the one holding the vault
    // session — a session lives in one browser's storage. Without something to copy, a pairing
    // started in the wrong browser is a dead end, which is exactly what happened.
    const link = await screen.findByLabelText(/Pairing link/u);
    expect(link).toHaveValue('https://vault.example.invalid/pair?code=synthetic-code');
    expect(screen.getByText('synthetic-code')).toBeVisible();

    await userEvent.click(screen.getByRole('button', { name: 'Copy link' }));
    await waitFor(() => {
      expect(writeText).toHaveBeenCalledWith(
        'https://vault.example.invalid/pair?code=synthetic-code',
      );
    });
    expect(await screen.findByRole('button', { name: 'Copied' })).toBeVisible();

    // The block used to live in .workflow-status, which is display:flex in row direction and
    // built for one short line. Every child became a squeezed column and the code broke one
    // character per line. Naming the container here is what stops that returning.
    const code = screen.getByText('synthetic-code');
    expect(code.closest('.workflow-status')).toBeNull();
    expect(code.closest('.settings-pairing')).not.toBeNull();
  };

  it('shows the fingerprint while waiting, because approving without it proves nothing', async () => {
    const gateway = makeGateway();
    render(<SettingsPanel gateway={gateway} />);
    await loadSettings();
    await openTab('Keyvault');

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
    await openTab('Keyvault');

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
    await openTab('Keyvault');

    await userEvent.click(screen.getByRole('button', { name: 'Test connection' }));

    expect(
      await screen.findByText('The token was refused — create a new one in the vault.'),
    ).toBeVisible();
  });
});

describe('expiryLabel', () => {
  it('counts down in words and says plainly when the code is spent', () => {
    const now = 1_775_000_000_000;
    expect(expiryLabel(now + 29 * 60_000, now)).toContain('29 more minutes');
    expect(expiryLabel(now + 90_000, now)).toContain('a minute more');
    // A dead code needs an instruction, not a number: the button is right there.
    expect(expiryLabel(now - 1, now)).toContain('Press Connect again');
    // Absent is not an error: the line simply is not shown, and the pairing is unaffected.
    expect(expiryLabel(null, now)).toBeNull();
  });

  it('saves the TypeSafe key and runs a scan with flags and delete', async () => {
    const user = userEvent.setup();
    const saveTypeSafeSettings = vi.fn(async () => ({ apiKey: 'apikey_test' }));
    const scanStatus = vi.fn(async () => ({
      runId: 'scan-1',
      state: 'completed' as const,
      processed: 40,
      total: 40,
      flagged: [
        { eventId: 7, preview: 'Moje hasło do banku: Kropka12!Malina', probability: 0.98 },
      ],
      errorCode: null,
    }));
    const scanStart = vi.fn(async () => ({ runId: 'scan-1' }));
    const deleteEvent = vi.fn(async () => undefined);
    const gateway = makeGateway({
      saveTypeSafeSettings,
      typesafeScanStart: scanStart,
      typesafeScanStatus: scanStatus,
      deleteEvent,
    });
    render(<SettingsPanel gateway={gateway} />);
    await loadSettings();

    // The tab strip reaches the privacy pane by its label.
    await openTab('Privacy');

    const key = screen.getByLabelText('TypeSafe API key');
    await user.clear(key);
    await user.type(key, 'apikey_test');
    await user.click(screen.getByRole('button', { name: 'Save key' }));
    expect(saveTypeSafeSettings).toHaveBeenCalledWith({ apiKey: 'apikey_test' });

    await user.click(screen.getByRole('button', { name: 'Scan history' }));
    expect(scanStart).toHaveBeenCalled();
    await waitFor(() => expect(screen.getByText(/entries scanned, 1 worth a look/u)).toBeVisible());

    await user.click(screen.getByRole('button', { name: 'Delete' }));
    expect(deleteEvent).toHaveBeenCalledWith(7);
  });
});
