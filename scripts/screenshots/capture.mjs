// Photographs the real Trove interface for the project site.
//
//   pnpm screenshots            # starts the Vite dev server if it is not running
//   pnpm screenshots -- --keep-png
//
// The interface runs in Chromium with `fake-tauri.js` injected ahead of it,
// so it talks to an invented core instead of the desktop shell: every entry,
// application, key name and chat line on the pictures is fictional. Windows
// are photographed at the sizes `src-tauri/tauri.conf.json` gives them, at
// 2× for Retina-sharp images, and written as WebP when `cwebp` is on the PATH
// (brew install webp); PNG otherwise.

import { spawn, spawnSync } from "node:child_process";
import { mkdir, readFile, rm, stat } from "node:fs/promises";
import { dirname, join } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

import { chromium } from "playwright";

const root = join(dirname(fileURLToPath(import.meta.url)), "..", "..");
const outDir = join(root, "docs", "assets", "screenshots");
const baseUrl = process.env.TROVE_UI_URL ?? "http://localhost:1420/";
const keepPng = process.argv.includes("--keep-png");
const fakeTauri = await readFile(join(dirname(fileURLToPath(import.meta.url)), "fake-tauri.js"), "utf8");

const PALETTE = { width: 1040, height: 680 };
const SETTINGS = { width: 840, height: 820 };
const CHAT = { width: 560, height: 720 };

const settle = (page, ms = 400) => page.waitForTimeout(ms);

/** Each shot: a window, a route, and the keys that bring the view up. */
const SHOTS = [
  { name: "palette-home", size: PALETTE, hash: "", steps: async () => {} },
  {
    name: "palette-history",
    size: PALETTE,
    hash: "",
    steps: async (page) => {
      await page.keyboard.press("2");
      await settle(page);
      await page.keyboard.press("ArrowDown");
    },
  },
  {
    name: "palette-link-preview",
    size: PALETTE,
    hash: "",
    steps: async (page) => {
      await page.keyboard.press("2");
      await settle(page);
      await page.keyboard.press("ArrowDown");
      await page.keyboard.press("ArrowDown");
    },
  },
  {
    name: "palette-image",
    size: PALETTE,
    hash: "",
    steps: async (page) => {
      await page.keyboard.press("2");
      await settle(page);
      for (let i = 0; i < 3; i++) await page.keyboard.press("ArrowDown");
    },
  },
  {
    name: "palette-search",
    size: PALETTE,
    hash: "",
    steps: async (page) => {
      await page.keyboard.press("2");
      await settle(page);
      await page.keyboard.type("wrzesien");
    },
  },
  {
    name: "palette-apps",
    size: PALETTE,
    hash: "",
    steps: async (page) => {
      await page.keyboard.press("1");
      await settle(page, 800);
    },
  },
  {
    name: "palette-vault",
    size: PALETTE,
    hash: "",
    steps: async (page) => {
      await page.keyboard.press("3");
    },
  },
  {
    name: "palette-windows",
    size: PALETTE,
    hash: "",
    steps: async (page) => {
      await page.keyboard.press("4");
    },
  },
  {
    name: "palette-import",
    size: PALETTE,
    hash: "",
    steps: async (page) => {
      await page.getByRole("button", { name: "Import an archive" }).click();
    },
  },
  {
    name: "chat",
    size: CHAT,
    hash: "#chat",
    steps: async (page) => {
      const message = page.getByRole("textbox", { name: "Message" });
      await message.fill(
        "Write a short release note for Trove 1.8: a Windows category that snaps the last window, and arrow keys on the home tiles.",
      );
      await message.press("Enter");
      await settle(page, 1500);
    },
  },
  { name: "settings-shortcut", size: SETTINGS, hash: "#settings", steps: async () => {} },
  {
    name: "settings-retention",
    size: SETTINGS,
    hash: "#settings",
    steps: async (page) => {
      await page.getByRole("tab", { name: "Retention" }).click();
    },
  },
  {
    name: "settings-apps",
    size: SETTINGS,
    hash: "#settings",
    steps: async (page) => {
      await page.getByRole("tab", { name: "Apps" }).click();
    },
  },
  {
    name: "settings-storage",
    size: SETTINGS,
    hash: "#settings",
    steps: async (page) => {
      await page.getByRole("tab", { name: "Storage" }).click();
    },
  },
];

const isUp = async () => {
  try {
    return (await fetch(baseUrl)).ok;
  } catch {
    return false;
  }
};

let server = null;
if (!(await isUp())) {
  server = spawn("pnpm", ["--dir", "apps/desktop-ui", "dev"], { cwd: root, stdio: "ignore" });
  for (let i = 0; i < 60 && !(await isUp()); i++) await new Promise((r) => setTimeout(r, 500));
  if (!(await isUp())) throw new Error(`the interface did not come up at ${baseUrl}`);
}

const hasCwebp = spawnSync("cwebp", ["-version"]).status === 0;
await mkdir(outDir, { recursive: true });

const browser = await chromium.launch();
const warnings = [];
try {
  for (const shot of SHOTS) {
    const context = await browser.newContext({
      viewport: shot.size,
      deviceScaleFactor: 2,
      colorScheme: "light",
      locale: "en-GB",
    });
    await context.addInitScript(fakeTauri);
    const page = await context.newPage();
    page.on("console", (message) => {
      if (message.type() === "error" || message.type() === "warning") {
        warnings.push(`${shot.name}: ${message.text()}`);
      }
    });
    page.on("pageerror", (error) => warnings.push(`${shot.name}: ${error.message}`));
    await page.goto(`${baseUrl}${shot.hash}`, { waitUntil: "networkidle" });
    await page.evaluate(() => document.fonts.ready);
    await settle(page, 600);
    await shot.steps(page);
    await settle(page, 700);

    const png = join(outDir, `${shot.name}.png`);
    await page.screenshot({ path: png });
    await context.close();

    if (hasCwebp) {
      const webp = join(outDir, `${shot.name}.webp`);
      spawnSync("cwebp", ["-quiet", "-q", "82", "-m", "6", png, "-o", webp], { stdio: "inherit" });
      if (!keepPng) await rm(png);
      console.log(`${shot.name}.webp  ${Math.round((await stat(webp)).size / 1024)} KB`);
    } else {
      console.log(`${shot.name}.png  ${Math.round((await stat(png)).size / 1024)} KB (no cwebp)`);
    }
  }

  // The Open Graph preview, composed from the icon and a fresh shot. PNG,
  // because link scrapers do not all read WebP.
  const og = await browser.newPage({ viewport: { width: 1200, height: 630 } });
  await og.goto(pathToFileURL(join(root, "scripts", "screenshots", "og-template.html")).href);
  await og.evaluate(() => document.fonts.ready);
  await og.screenshot({ path: join(root, "docs", "assets", "og-image.png") });
  await og.close();
  console.log("og-image.png");
} finally {
  await browser.close();
  server?.kill();
}

if (warnings.length) {
  console.log("\nConsole warnings and errors:");
  for (const line of warnings) console.log(`  ${line}`);
}
