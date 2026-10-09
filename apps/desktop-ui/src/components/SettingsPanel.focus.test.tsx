import "@testing-library/jest-dom/vitest";
import { act, render, screen } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

import type { AppSettings } from "../lib/contracts";
import type { ClipboardGateway } from "../lib/gateway";
import { defaultSnapShortcuts } from "../lib/snapShortcuts";
import { SettingsPanel } from "./SettingsPanel";

/// The Tauri window, replaced, and kept apart from the other settings tests:
/// those render with no window at all, the way the browser preview does.
/// Here the window exists and starts hidden, which is how the shell creates
/// it at every launch.
type FocusListener = (event: { payload: boolean }) => void;
const focusListeners: FocusListener[] = [];

vi.mock("@tauri-apps/api/window", () => ({
  getCurrentWindow: () => ({
    isVisible: () => Promise.resolve(false),
    onFocusChanged: (listener: FocusListener) => {
      focusListeners.push(listener);
      return Promise.resolve(() => {
        const index = focusListeners.indexOf(listener);
        if (index >= 0) focusListeners.splice(index, 1);
      });
    },
    close: () => Promise.resolve(),
  }),
}));

const gainFocus = async (): Promise<void> => {
  await act(async () => {
    for (const listener of [...focusListeners]) listener({ payload: true });
  });
};

const persistedSettings: AppSettings = {
  schemaVersion: 1,
  hotkey: "CommandOrControl+Space",
  autostart: false,
  paletteModes: true,
  dockIcon: false,
  retentionDays: 30,
  denylistedApps: [],
  linkPreviews: true,
  snapShortcuts: defaultSnapShortcuts(),
  keyvault: { url: null, token: null, privateJwk: null },
};

const makeGateway = () => {
  const getStorageStats = vi.fn(async () => ({
    contentCount: 3,
    eventCount: 4,
    databaseBytes: 1_024,
    blobBytes: 0,
  }));
  const getShortcutStatus = vi.fn(async () => ({
    hotkey: "CommandOrControl+Space",
    registered: true,
    heldBySystem: false,
    releasedIds: [],
  }));
  const gateway = {
    getSettings: vi.fn(async () => persistedSettings),
    isAutostartEnabled: vi.fn(async () => false),
    keyvaultIdentity: vi.fn(async () => ({ paired: false, url: null })),
    getStorageStats,
    getShortcutStatus,
  } as unknown as ClipboardGateway;
  return { gateway, getStorageStats, getShortcutStatus };
};

describe("SettingsPanel in a hidden window", () => {
  beforeEach(() => {
    focusListeners.length = 0;
  });

  it("leaves the storage figures and the shortcut alone until the window is shown", async () => {
    const { gateway, getStorageStats, getShortcutStatus } = makeGateway();
    render(<SettingsPanel gateway={gateway} />);
    await screen.findByRole("textbox", { name: "Global shortcut" });

    // Mounted hidden at launch: the launch must not wait behind a full-table
    // count and a read of the system's shortcut table nobody will see.
    expect(getStorageStats).not.toHaveBeenCalled();
    expect(getShortcutStatus).not.toHaveBeenCalled();

    await gainFocus();

    expect(getStorageStats).toHaveBeenCalledTimes(1);
    expect(getShortcutStatus).toHaveBeenCalledTimes(1);
  });

  it("reads them again every time the window comes forward", async () => {
    const { gateway, getStorageStats, getShortcutStatus } = makeGateway();
    render(<SettingsPanel gateway={gateway} />);
    await screen.findByRole("textbox", { name: "Global shortcut" });

    await gainFocus();
    await gainFocus();

    // The history grows while the window is away, and the chord can be taken
    // by another application at any time; a figure read once is stale.
    expect(getStorageStats).toHaveBeenCalledTimes(2);
    expect(getShortcutStatus).toHaveBeenCalledTimes(2);
  });
});
