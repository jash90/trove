import "@testing-library/jest-dom/vitest";
import {
  fireEvent,
  render,
  screen,
  waitFor,
  within,
} from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { App, isSummoningShortcut, shortcutHint } from "./App";
import { mockGateway, type ClipboardGateway } from "./lib/gateway";
import {
  SYNTHETIC_APPS,
  SYNTHETIC_KEYVAULT_SECRETS,
  SYNTHETIC_SETTINGS,
} from "./lib/fixtures";

it("renders the private clipboard palette landmark", () => {
  render(<App />);
  expect(
    screen.getByRole("application", { name: "Clipboard palette" }),
  ).toBeVisible();
});

it("opens the import wizard over the palette and returns focus to the search field", async () => {
  const user = userEvent.setup();
  render(<App />);
  const search = screen.getByRole("searchbox");

  await user.click(screen.getByRole("button", { name: "Import an archive" }));
  expect(screen.getByRole("dialog", { name: "Import history" })).toBeVisible();
  expect(screen.getByLabelText("Trove palette")).toHaveAttribute("inert");

  await user.click(screen.getByRole("button", { name: "Close import" }));
  // The palette has one place a keyboard user works from, and a shortcut has
  // no button to hand focus back to.
  await waitFor(() => expect(search).toHaveFocus());
  expect(screen.getByLabelText("Trove palette")).not.toHaveAttribute("inert");
});

it("asks for the settings window rather than covering the list with a dialog", async () => {
  const user = userEvent.setup();
  const openSettingsWindow = vi.fn(async () => undefined);
  render(<App gateway={{ ...mockGateway, openSettingsWindow }} />);

  await user.click(screen.getByRole("button", { name: "Open settings" }));
  expect(openSettingsWindow).toHaveBeenCalledOnce();

  await user.keyboard("{Meta>},{/Meta}");
  expect(openSettingsWindow).toHaveBeenCalledTimes(2);
  // Nothing was drawn over the palette either way.
  expect(
    screen.queryByRole("dialog", { name: "Settings" }),
  ).not.toBeInTheDocument();
});

it("opens the import wizard from the keyboard, with no history selected", async () => {
  const user = userEvent.setup();
  render(<App />);

  await user.keyboard("{Meta>}i{/Meta}");
  expect(
    await screen.findByRole("dialog", { name: "Import history" }),
  ).toBeVisible();
});

describe("the summoning shortcut inside the palette", () => {
  const press = (
    mods: Partial<
      Record<"metaKey" | "ctrlKey" | "altKey" | "shiftKey", boolean>
    >,
  ) => ({
    code: "Space",
    metaKey: false,
    ctrlKey: false,
    altKey: false,
    shiftKey: false,
    ...mods,
  });

  it("recognises the configured chord so the palette can put itself away", () => {
    // Pressing it while the search field had focus did nothing: the chord went
    // into the text field rather than through it, and the one key meant to be
    // in charge of both directions only worked in one.
    expect(
      isSummoningShortcut(press({ metaKey: true }), "CommandOrControl+Space"),
    ).toBe(true);
  });

  it("follows a rebound shortcut rather than a hardcoded one", () => {
    expect(
      isSummoningShortcut(
        press({ metaKey: true }),
        "CommandOrControl+Shift+Space",
      ),
    ).toBe(false);
    expect(
      isSummoningShortcut(
        press({ metaKey: true, shiftKey: true }),
        "CommandOrControl+Shift+Space",
      ),
    ).toBe(true);
  });

  it("leaves an ordinary space alone", () => {
    // Typing a space in the search field must stay a space.
    expect(isSummoningShortcut(press({}), "CommandOrControl+Space")).toBe(
      false,
    );
  });

  it("does nothing until the configured shortcut has been read", () => {
    expect(isSummoningShortcut(press({ metaKey: true }), null)).toBe(false);
  });

  it("names the shortcut in the footer the way a keyboard is drawn", () => {
    // Hardcoded prose went on advertising ⌘⇧Space after the shortcut changed.
    expect(shortcutHint("CommandOrControl+Space")).toBe("⌘Space");
    expect(shortcutHint("Control+Alt+7")).toBe("⌃⌥7");
    expect(shortcutHint(null)).toBe("");
  });
});

describe("the unified palette", () => {
  beforeEach(() => {
    // jsdom reports zero-sized elements, so the virtualizer would render no rows.
    vi.spyOn(HTMLElement.prototype, "offsetWidth", "get").mockReturnValue(900);
    vi.spyOn(HTMLElement.prototype, "offsetHeight", "get").mockReturnValue(480);
  });

  afterEach(() => {
    vi.restoreAllMocks();
  });

  const historyResults = () =>
    within(
      screen.getByRole("listbox", {
        name: "Applications, secrets and history results",
      }),
    );
  const appsResults = () =>
    within(
      screen.getByRole("listbox", {
        name: "Applications, secrets and history results",
      }),
    );

  // The palette opens on its category chooser; a test that wants a list
  // picks its category the way a user does — the digit on the tile.
  const pick = (key: string): void => {
    fireEvent.keyDown(screen.getByRole("searchbox"), { key });
  };
  const homeTiles = () =>
    screen.getAllByRole("button", {
      name: /Applications|Clipboard history|Key vault|Windows|Chat/u,
    });
  const settleHistory = async (): Promise<void> => {
    fireEvent.keyDown(screen.getByRole("searchbox"), {
      key: "2",
      metaKey: true,
    });
    await waitFor(() =>
      expect(historyResults().getAllByRole("option").length).toBeGreaterThan(0),
    );
  };
  const settleApps = async (): Promise<void> => {
    fireEvent.keyDown(screen.getByRole("searchbox"), {
      key: "1",
      metaKey: true,
    });
    await waitFor(() =>
      expect(appsResults().getAllByRole("option").length).toBeGreaterThan(0),
    );
  };

  it("opens on its categories, and typing means the history", async () => {
    const user = userEvent.setup();
    render(<App />);

    // The palette opens on the chooser: three categories, no list rows —
    // which list someone came for is a fact about them, not the app.
    await waitFor(() => expect(homeTiles()).toHaveLength(5));
    expect(
      screen.queryByRole("listbox", {
        name: "Applications, secrets and history results",
      }),
    ).toBeNull();

    // Typing from home enters the history with the query already carried.
    await user.type(screen.getByRole("searchbox"), "project note");
    await waitFor(() =>
      expect(historyResults().getAllByRole("option").length).toBeGreaterThan(0),
    );
    expect(
      historyResults()
        .getAllByRole("option")
        .filter((option) => option.hasAttribute("data-path")),
    ).toHaveLength(0);
  });

  it("opens chat with the categories turned off, where there are no tiles", async () => {
    // The combined list has no chooser, so the chat reaches it by key and
    // by the footer — the window is a destination, not a category.
    const user = userEvent.setup();
    const openChatWindow = vi.fn(async () => undefined);
    const getSettings = vi.fn(async () => ({
      ...SYNTHETIC_SETTINGS,
      paletteModes: false,
    }));
    render(
      <App
        gateway={
          { ...mockGateway, openChatWindow, getSettings } as ClipboardGateway
        }
      />,
    );
    // The combined list answers: rows without picking anything.
    await waitFor(() =>
      expect(historyResults().getAllByRole("option").length).toBeGreaterThan(0),
    );

    await user.keyboard("{Meta>}5{/Meta}");
    expect(openChatWindow).toHaveBeenCalledOnce();

    await user.keyboard("{Meta>}k{/Meta}");
    expect(openChatWindow).toHaveBeenCalledTimes(2);

    await user.click(
      screen.getByRole("button", { name: "Open the chat window" }),
    );
    expect(openChatWindow).toHaveBeenCalledTimes(3);
  });

  it("opens the chat window as the fifth tile", async () => {
    const openChatWindow = vi.fn(async () => undefined);
    render(
      <App gateway={{ ...mockGateway, openChatWindow } as ClipboardGateway} />,
    );
    await waitFor(() => expect(homeTiles()).toHaveLength(5));

    fireEvent.keyDown(screen.getByRole("searchbox"), { key: "5" });

    expect(openChatWindow).toHaveBeenCalledOnce();
    // Opening the chat window is a destination, not a mode: the chooser
    // stays where it was.
    expect(homeTiles()).toHaveLength(5);
  });

  it("opens the Windows category with its digit and walks it with Enter", async () => {
    const snapWindow = vi.fn(async () => true);
    const user = userEvent.setup();
    render(
      <App
        gateway={{ ...mockGateway, snapWindow } as ClipboardGateway}
      />,
    );
    await waitFor(() => expect(homeTiles()).toHaveLength(5));

    // The fourth tile is the Windows category.
    fireEvent.keyDown(screen.getByRole("searchbox"), { key: "4" });
    const rows = screen.getAllByRole("option");
    expect(rows).toHaveLength(10);
    expect(rows[0]).toHaveTextContent("Left half");
    // The chords the settings hold are the ones the rows wear.
    expect(rows[0]).toHaveTextContent("CommandOrControl+Alt+ArrowLeft");

    // Enter commits the first arrangement to the gateway.
    await user.keyboard("{Enter}");
    expect(snapWindow).toHaveBeenCalledWith("leftHalf");
  });

  it("keeps the palette and explains when a snap finds no window", async () => {
    const snapWindow = vi.fn(async () => false);
    const user = userEvent.setup();
    render(
      <App
        gateway={{ ...mockGateway, snapWindow } as ClipboardGateway}
      />,
    );
    await waitFor(() => expect(homeTiles()).toHaveLength(5));

    fireEvent.keyDown(screen.getByRole("searchbox"), { key: "4" });
    await user.keyboard("{Enter}");

    expect(snapWindow).toHaveBeenCalledWith("leftHalf");
    expect(
      await screen.findByRole("alert"),
    ).toHaveTextContent("That window could not be arranged.");
    // Still in the category — a refused snap is not a reason to leave it.
    expect(screen.getAllByRole("option")).toHaveLength(10);
  });

  it("picks a category with its tile digit, in tile order", async () => {
    render(<App />);
    await waitFor(() => expect(homeTiles()).toHaveLength(5));

    pick("1");
    await waitFor(() =>
      expect(
        appsResults()
          .getAllByRole("option")
          .filter((option) => option.hasAttribute("data-path")),
      ).toHaveLength(SYNTHETIC_APPS.length),
    );
    expect(
      appsResults()
        .getAllByRole("option")
        .filter((option) => option.hasAttribute("data-event-id")),
    ).toHaveLength(0);

    // Escape clears the query; the second Escape is the way back home.
    fireEvent.keyDown(screen.getByRole("searchbox"), { key: "Escape" });
    fireEvent.keyDown(screen.getByRole("searchbox"), { key: "Escape" });
    await waitFor(() => expect(homeTiles()).toHaveLength(5));
  });

  it("walks the tiles with the arrows and commits with Enter", async () => {
    render(<App />);
    await waitFor(() => expect(homeTiles()).toHaveLength(5));

    const search = screen.getByRole("searchbox");
    fireEvent.keyDown(search, { key: "ArrowDown" });
    expect(homeTiles()[0]).toHaveAttribute("data-selected", "true");
    fireEvent.keyDown(search, { key: "ArrowDown" });
    expect(homeTiles()[1]).toHaveAttribute("data-selected", "true");

    // Enter takes the walked-to tile — the second one is the history.
    fireEvent.keyDown(search, { key: "Enter" });
    await waitFor(() =>
      expect(historyResults().getAllByRole("option").length).toBeGreaterThan(0),
    );
  });

  it("wraps the arrow walk in both directions", async () => {
    render(<App />);
    await waitFor(() => expect(homeTiles()).toHaveLength(5));

    const search = screen.getByRole("searchbox");
    // Up from nothing starts at the end — the chat tile; down wraps to the first.
    fireEvent.keyDown(search, { key: "ArrowUp" });
    expect(homeTiles()[4]).toHaveAttribute("data-selected", "true");
    fireEvent.keyDown(search, { key: "ArrowDown" });
    expect(homeTiles()[0]).toHaveAttribute("data-selected", "true");
  });

  it("opens the chat window from an arrow walk and Enter", async () => {
    const openChatWindow = vi.fn(async () => undefined);
    render(
      <App gateway={{ ...mockGateway, openChatWindow } as ClipboardGateway} />,
    );
    await waitFor(() => expect(homeTiles()).toHaveLength(5));

    const search = screen.getByRole("searchbox");
    fireEvent.keyDown(search, { key: "End" });
    expect(homeTiles()[4]).toHaveAttribute("data-selected", "true");
    fireEvent.keyDown(search, { key: "Enter" });

    expect(openChatWindow).toHaveBeenCalledOnce();
  });

  it("shows the keys of the paired vault in its category", async () => {
    const user = userEvent.setup();
    const keyvaultCopySecret = vi.fn(async () => undefined);
    render(
      <App
        gateway={{ ...mockGateway, keyvaultCopySecret } as ClipboardGateway}
      />,
    );

    pick("3");
    const vaultRows = await waitFor(() => {
      const rows = historyResults().getAllByRole("option");
      expect(rows.length).toBeGreaterThanOrEqual(
        SYNTHETIC_KEYVAULT_SECRETS.length,
      );
      return rows;
    });
    expect(vaultRows.some((row) => row.textContent?.includes("openai"))).toBe(
      true,
    );

    // Enter copies the selected key through the core — the value never
    // crosses the interface — and the palette lands back on the categories.
    await user.keyboard("{Enter}");
    expect(keyvaultCopySecret).toHaveBeenCalledWith("openai");
    await waitFor(() => expect(homeTiles()).toHaveLength(5));
  });

  it("picks a category directly with ⌘1, ⌘2 and ⌘3, with no header control left", async () => {
    const user = userEvent.setup();
    render(<App />);

    await user.keyboard("{Meta>}1{/Meta}");
    await waitFor(() =>
      expect(
        historyResults()
          .getAllByRole("option")
          .filter((option) => option.hasAttribute("data-path")).length,
      ).toBeGreaterThan(0),
    );

    await user.keyboard("{Meta>}2{/Meta}");
    await waitFor(() =>
      expect(
        historyResults()
          .getAllByRole("option")
          .filter((option) => option.hasAttribute("data-event-id")).length,
      ).toBeGreaterThan(0),
    );

    await user.keyboard("{Meta>}3{/Meta}");
    await waitFor(() =>
      expect(
        historyResults()
          .getAllByRole("option")
          .some((option) => option.textContent?.includes("openai")),
      ).toBe(true),
    );

    // The header carries no category control: the tiles and their keys are
    // the one way in, and the top of the palette is the field.
    expect(
      screen.queryByRole("group", { name: "Palette categories" }),
    ).toBeNull();
    expect(screen.queryByRole("button", { name: "Apps" })).toBeNull();
  });

  it("returns to the categories after launching an application", async () => {
    const user = userEvent.setup();
    const launchApp = vi.fn(async () => undefined);
    render(<App gateway={{ ...mockGateway, launchApp } as ClipboardGateway} />);

    await user.keyboard("{Meta>}1{/Meta}");
    await user.type(screen.getByRole("searchbox"), "terminal");
    await waitFor(() =>
      expect(appsResults().getAllByRole("option")).toHaveLength(1),
    );
    await user.keyboard("{Enter}");

    expect(launchApp).toHaveBeenCalled();
    // The stay in a category ended with the errand: the next summoning
    // opens back on the categories.
    await waitFor(() => expect(homeTiles()).toHaveLength(5));
  });

  it("filters applications client-side while typing once fetched", async () => {
    const user = userEvent.setup();
    const listApps = vi.fn(async () =>
      SYNTHETIC_APPS.map((app) => ({ ...app })),
    );
    render(<App gateway={{ ...mockGateway, listApps } as ClipboardGateway} />);

    await settleApps();

    await user.type(screen.getByRole("searchbox"), "notes");
    await waitFor(() =>
      expect(appsResults().getAllByRole("option")).toHaveLength(1),
    );
    expect(
      appsResults().getByRole("option", { selected: true }),
    ).toHaveTextContent("Synthetic Notes");

    // Every keystroke after the first narrowed a list already on the client.
    expect(listApps).toHaveBeenCalledOnce();
  });

  it("launches the selected application with Enter", async () => {
    const user = userEvent.setup();
    const launchApp = vi.fn(async () => undefined);
    render(<App gateway={{ ...mockGateway, launchApp } as ClipboardGateway} />);

    await settleApps();

    await user.type(screen.getByRole("searchbox"), "terminal");
    await waitFor(() =>
      expect(appsResults().getAllByRole("option")).toHaveLength(1),
    );
    await user.keyboard("{Enter}");

    expect(launchApp).toHaveBeenCalledWith(
      "/synthetic/Applications/Utilities/Synthetic Terminal.app",
    );
  });

  it("pastes the selected history entry when no application matches", async () => {
    const user = userEvent.setup();
    const copyEvent = vi.fn(async () => ({ mode: "pasted", plainText: false }));
    render(<App gateway={{ ...mockGateway, copyEvent } as ClipboardGateway} />);

    await settleApps();
    await settleHistory();

    // A query no application matches: every row left is history, so the
    // first selectable one pastes.
    await user.type(screen.getByRole("searchbox"), "project note");
    await waitFor(() =>
      expect(
        historyResults()
          .getAllByRole("option")
          .filter((option) => option.hasAttribute("data-path")),
      ).toHaveLength(0),
    );
    await waitFor(() =>
      expect(historyResults().getAllByRole("option").length).toBeGreaterThan(0),
    );

    await user.keyboard("{Enter}");

    expect(copyEvent).toHaveBeenCalledWith(expect.any(Number), false, true);
  });

  it("names the Accessibility refusal and offers the way to fix it", async () => {
    const user = userEvent.setup();
    const copyEvent = vi.fn(async () => ({
      mode: "copied_only_permission_required",
      plainText: false,
    }));
    const openAccessibilitySettings = vi.fn(async () => undefined);
    render(
      <App
        gateway={
          {
            ...mockGateway,
            copyEvent,
            openAccessibilitySettings,
          } as ClipboardGateway
        }
      />,
    );

    await settleApps();
    await settleHistory();

    await user.type(screen.getByRole("searchbox"), "project note");
    await waitFor(() =>
      expect(historyResults().getAllByRole("option").length).toBeGreaterThan(0),
    );
    await user.keyboard("{Enter}");

    // The three refusals used to share one sentence, which hid the only one
    // the user can act on behind the two they cannot.
    await waitFor(() =>
      expect(
        screen.getByText(/Pasting needs Accessibility permission/),
      ).toBeInTheDocument(),
    );

    await user.click(
      screen.getByRole("button", { name: "Open System Settings" }),
    );
    expect(openAccessibilitySettings).toHaveBeenCalled();
  });

  it("offers no fix for a refusal the user cannot act on", async () => {
    const user = userEvent.setup();
    const copyEvent = vi.fn(async () => ({
      mode: "copied_only_target_lost",
      plainText: false,
    }));
    render(<App gateway={{ ...mockGateway, copyEvent } as ClipboardGateway} />);

    await settleApps();
    await settleHistory();

    await user.type(screen.getByRole("searchbox"), "project note");
    await waitFor(() =>
      expect(historyResults().getAllByRole("option").length).toBeGreaterThan(0),
    );
    await user.keyboard("{Enter}");

    await waitFor(() =>
      expect(
        screen.getByText(/The window you were in is no longer there/),
      ).toBeInTheDocument(),
    );
    expect(
      screen.queryByRole("button", { name: "Open System Settings" }),
    ).not.toBeInTheDocument();
  });

  it("opens the preview only for a selected history entry", async () => {
    const user = userEvent.setup();
    render(<App />);

    await settleHistory();

    // The palette opens with a history row selected, and the pane opens
    // for it: the history is the default mode. (The same text sits in the
    // row and in the pane, so the query counts rather than finds one.)
    await waitFor(() =>
      expect(
        screen.getAllByText("Synthetic project note for browser preview")
          .length,
      ).toBeGreaterThan(0),
    );

    // An application holds the selection in applications category; an
    // application has no payload to preview, so the pane shows nothing.
    await user.keyboard("{Meta>}1{/Meta}");
    await waitFor(() => {
      expect(
        historyResults()
          .getAllByRole("option")
          .some((option) => option.hasAttribute("data-path")),
      ).toBe(true);
      expect(
        screen.queryAllByText("Synthetic project note for browser preview"),
      ).toHaveLength(0);
    });
  });

  it("keeps the row shortcuts of the history inert while an application is selected", async () => {
    const user = userEvent.setup();
    const setPinned = vi.fn(async () => undefined);
    const deleteEvent = vi.fn(async () => undefined);
    render(
      <App
        gateway={{ ...mockGateway, setPinned, deleteEvent } as ClipboardGateway}
      />,
    );

    await settleApps();

    await user.type(screen.getByRole("searchbox"), "terminal");
    await waitFor(() =>
      expect(appsResults().getAllByRole("option")).toHaveLength(1),
    );

    await user.keyboard("{Meta>}p{/Meta}");
    await user.keyboard("{Delete}");
    await user.keyboard("{Meta>}c{/Meta}");

    expect(setPinned).not.toHaveBeenCalled();
    expect(deleteEvent).not.toHaveBeenCalled();
    expect(screen.queryByRole("dialog")).toBeNull();
  });

  it("cycles the categories with Tab, and Shift+Tab still leaves the field", async () => {
    const user = userEvent.setup();
    render(<App />);

    const search = screen.getByRole("searchbox");
    expect(search).toHaveFocus();

    // home → applications → history → vault → home: one ring, in tile
    // order, the field keeping the caret the whole way.
    await user.keyboard("{Tab}");
    await waitFor(() =>
      expect(
        historyResults()
          .getAllByRole("option")
          .some((option) => option.hasAttribute("data-path")),
      ).toBe(true),
    );

    await user.keyboard("{Tab}");
    await waitFor(() =>
      expect(
        historyResults()
          .getAllByRole("option")
          .some((option) => option.hasAttribute("data-event-id")),
      ).toBe(true),
    );

    await user.keyboard("{Tab}");
    await waitFor(() =>
      expect(
        historyResults()
          .getAllByRole("option")
          .some((option) => option.textContent?.includes("openai")),
      ).toBe(true),
    );

    // The fourth Tab passes through the Windows category — ten
    // arrangements, no query — on its way back to the chooser.
    await user.keyboard("{Tab}");
    await waitFor(() =>
      expect(screen.getAllByRole("option")).toHaveLength(10),
    );

    await user.keyboard("{Tab}");
    await waitFor(() => expect(homeTiles()).toHaveLength(5));
    expect(search).toHaveFocus();

    // Shift+Tab keeps the browser's meaning — the keyboard route out of
    // the field to the controls below survives the Tab override.
    await user.keyboard("{Shift>}{Tab}{/Shift}");
    expect(search).not.toHaveFocus();
  });

  it("backs out with Escape one step at a time: query, then category, then home", async () => {
    const user = userEvent.setup();
    render(<App />);

    await user.keyboard("{Meta>}1{/Meta}");
    await user.type(screen.getByRole("searchbox"), "ter");

    await user.keyboard("{Escape}");
    // The query went first; the applications category is still on.
    expect(screen.getByRole("searchbox")).toHaveValue("");
    expect(
      historyResults()
        .getAllByRole("option")
        .some((option) => option.hasAttribute("data-path")),
    ).toBe(true);

    await user.keyboard("{Escape}");
    // The second Escape left the category and returned to the chooser.
    await waitFor(() => expect(homeTiles()).toHaveLength(5));

    // A third Escape has nothing left to back out of, and hides nothing:
    // hiding the palette is the shortcut's job, as it always was.
    await user.keyboard("{Escape}");
    expect(
      screen.getByRole("application", { name: "Clipboard palette" }),
    ).toBeVisible();
    expect(homeTiles()).toHaveLength(5);
  });

  it("clears the query with Escape and never closes the palette", async () => {
    const user = userEvent.setup();
    render(<App />);
    await settleHistory();

    await user.type(screen.getByRole("searchbox"), "notes");
    await user.keyboard("{Escape}");

    expect(screen.getByRole("searchbox")).toHaveValue("");
    expect(
      screen.getByRole("application", { name: "Clipboard palette" }),
    ).toBeVisible();
    expect(historyResults().getAllByRole("option").length).toBeGreaterThan(0);
  });

  it("reports a failed launch without repeating the path it was given", async () => {
    const user = userEvent.setup();
    const launchApp = vi.fn(async () => {
      throw new Error("/private/wherever/the/app/was.app");
    });
    render(<App gateway={{ ...mockGateway, launchApp } as ClipboardGateway} />);

    await settleApps();

    await user.type(screen.getByRole("searchbox"), "terminal");
    await waitFor(() =>
      expect(appsResults().getAllByRole("option")).toHaveLength(1),
    );
    await user.keyboard("{Enter}");

    const alert = await screen.findByRole("alert");
    expect(alert).toHaveTextContent(/could not be started/i);
    expect(alert).not.toHaveTextContent("/private");
    expect(alert).not.toHaveTextContent(".app");
  });

  it("drops the launch alert when the user moves to a history row", async () => {
    const user = userEvent.setup();
    const launchApp = vi.fn(async () => {
      throw new Error("/private/wherever/the/app/was.app");
    });
    render(<App gateway={{ ...mockGateway, launchApp } as ClipboardGateway} />);

    await settleApps();

    await user.type(screen.getByRole("searchbox"), "terminal");
    await waitFor(() =>
      expect(appsResults().getAllByRole("option")).toHaveLength(1),
    );
    await user.keyboard("{Enter}");
    expect(await screen.findByRole("alert")).toBeVisible();

    // One list means one selection: a refusal that belongs to an application
    // row has no business staying on screen once the user is reading history.
    await user.keyboard("{Escape}");
    // First Escape cleared the query; the launch already returned the
    // palette to the chooser, so enter the history from there.
    await user.keyboard("{Meta>}2{/Meta}");
    const historyRow = await waitFor(() => {
      const row = historyResults()
        .getAllByRole("option")
        .find((option) => option.hasAttribute("data-event-id"));
      expect(row).toBeDefined();
      return row as HTMLElement;
    });
    await user.click(historyRow);

    await waitFor(() =>
      expect(screen.queryByRole("alert")).not.toBeInTheDocument(),
    );
  });

  it("leaves Tab to the dialog while one is open", async () => {
    const user = userEvent.setup();
    render(<App />);
    await settleHistory();

    await user.keyboard("{Meta>}i{/Meta}");
    expect(
      await screen.findByRole("dialog", { name: "Import history" }),
    ).toBeVisible();

    await user.keyboard("{Tab}");

    expect(
      screen.getByRole("listbox", {
        name: "Applications, secrets and history results",
      }),
    ).toBeInTheDocument();
    expect(
      screen.getByRole("dialog", { name: "Import history" }),
    ).toBeVisible();
  });
});
