// Checks the project site in docs/ the way a visitor meets it.
//
//   node scripts/screenshots/check-site.mjs http://localhost:48123/trove/
//
// Serve docs/ under a /trove/ prefix first, so relative paths are tested as
// GitHub Pages will serve them, e.g.:
//   mkdir -p /tmp/pages && ln -sfn "$PWD/docs" /tmp/pages/trove
//   python3 -m http.server 48123 -d /tmp/pages
//
// Both languages, desktop and phone widths, light and dark: console errors,
// images that fail to load, internal links that do not resolve, the language
// switch, hreflang, horizontal overflow, the lightbox and an axe-core pass.
// Full-page captures go to .playwright-mcp/ (git-ignored) for a visual look.

import { mkdir, readFile } from "node:fs/promises";
import { createRequire } from "node:module";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

import { chromium } from "playwright";

const root = join(dirname(fileURLToPath(import.meta.url)), "..", "..");
const base = process.argv[2] ?? "http://localhost:48123/trove/";
const outDir = join(root, ".playwright-mcp", "site-check");
const require = createRequire(join(root, "apps", "desktop-ui", "package.json"));
const axeSource = await readFile(require.resolve("axe-core/axe.min.js"), "utf8");

const PAGES = [
  { lang: "pl", url: base, other: new URL("en/", base).href },
  { lang: "en", url: new URL("en/", base).href, other: base },
];
const VIEWPORTS = [
  { name: "desktop", width: 1440, height: 900 },
  { name: "mobile", width: 390, height: 844 },
];

const failures = [];
const fail = (where, message) => failures.push(`${where}: ${message}`);

await mkdir(outDir, { recursive: true });
const browser = await chromium.launch();
const checkedLinks = new Map();

for (const target of PAGES) {
  for (const viewport of VIEWPORTS) {
    for (const scheme of ["light", "dark"]) {
      const where = `${target.lang}/${viewport.name}/${scheme}`;
      const context = await browser.newContext({ viewport, colorScheme: scheme });
      const page = await context.newPage();
      page.on("console", (m) => m.type() === "error" && fail(where, `console: ${m.text()}`));
      page.on("pageerror", (e) => fail(where, `pageerror: ${e.message}`));
      page.on("requestfailed", (r) => fail(where, `request failed: ${r.url()}`));
      page.on("response", (r) => r.status() >= 400 && fail(where, `HTTP ${r.status()}: ${r.url()}`));

      await page.goto(target.url, { waitUntil: "networkidle" });

      const lang = await page.getAttribute("html", "lang");
      if (lang !== target.lang) fail(where, `html lang is ${lang}`);
      const theme = await page.getAttribute("html", "data-theme");
      if (theme !== scheme) fail(where, `theme ${theme}, expected ${scheme} from the system`);

      // Bring every lazy image into view and wait for it, one at a time.
      const images = await page.locator("img").all();
      for (const img of images) {
        await img.scrollIntoViewIfNeeded().catch(() => undefined);
        await img
          .evaluate(
            (el) =>
              el.complete ||
              new Promise((r) => {
                el.addEventListener("load", r, { once: true });
                el.addEventListener("error", r, { once: true });
                setTimeout(r, 5000);
              }),
          )
          .catch(() => undefined);
      }
      await page.evaluate(() => window.scrollTo(0, 0));
      const broken = await page.evaluate(() =>
        [...document.images]
          .filter((img) => !img.src.startsWith("data:"))
          .filter((img) => img.checkVisibility())
          .filter((img) => !img.complete || img.naturalWidth === 0)
          .map((img) => img.src),
      );
      broken.forEach((src) => fail(where, `image not loaded: ${src}`));

      const missingAlt = await page.evaluate(
        () => [...document.images].filter((img) => !img.hasAttribute("alt")).length,
      );
      if (missingAlt) fail(where, `${missingAlt} images without alt`);

      const overflow = await page.evaluate(() => document.documentElement.scrollWidth - window.innerWidth);
      if (overflow > 0) fail(where, `horizontal overflow of ${overflow}px`);

      const hreflang = await page.$$eval('link[rel="alternate"][hreflang]', (links) =>
        links.map((l) => `${l.hreflang}=${l.href}`),
      );
      if (hreflang.length !== 3) fail(where, `expected 3 hreflang links, got ${hreflang.length}`);

      const switchHref = await page.$eval(".lang-switch", (a) => a.href);
      if (switchHref !== target.other) fail(where, `language switch goes to ${switchHref}`);

      // Internal links and in-page anchors.
      const links = await page.$$eval("a[href]", (as) => as.map((a) => a.href));
      for (const href of links) {
        const url = new URL(href);
        if (url.origin !== new URL(base).origin) continue;
        if (url.hash && url.pathname === new URL(target.url).pathname) {
          const exists = await page.$(url.hash);
          if (!exists) fail(where, `anchor ${url.hash} has no target`);
          continue;
        }
        const key = url.href.split("#")[0];
        if (!checkedLinks.has(key)) checkedLinks.set(key, (await fetch(key)).status);
        if (checkedLinks.get(key) !== 200) fail(where, `link ${key} → ${checkedLinks.get(key)}`);
      }

      if (viewport.name === "desktop" && scheme === "light") {
        // Lightbox: open, move, close with Escape, focus returns.
        const first = page.locator("[data-lightbox]").first();
        await first.scrollIntoViewIfNeeded();
        await first.click();
        const dialog = page.locator("dialog.lightbox");
        if (!(await dialog.evaluate((d) => d.open))) fail(where, "lightbox did not open");
        const before = await dialog.locator("img").getAttribute("src");
        await page.keyboard.press("ArrowRight");
        const after = await dialog.locator("img").getAttribute("src");
        if (before === after) fail(where, "ArrowRight did not move the lightbox");
        const loaded = await dialog
          .locator("img")
          .evaluate((img) => new Promise((r) => (img.complete ? r(img.naturalWidth > 0) : (img.onload = () => r(true)))));
        if (!loaded) fail(where, "lightbox image did not load");
        await page.keyboard.press("Escape");
        if (await dialog.evaluate((d) => d.open)) fail(where, "Escape did not close the lightbox");
        const focused = await page.evaluate(() => document.activeElement?.dataset?.lightbox ?? null);
        if (!focused) fail(where, "focus did not return to the gallery button");

        // Theme toggle flips and persists.
        await page.click(".theme-toggle");
        if ((await page.getAttribute("html", "data-theme")) !== "dark") fail(where, "toggle did not switch to dark");
        await page.reload({ waitUntil: "networkidle" });
        if ((await page.getAttribute("html", "data-theme")) !== "dark") fail(where, "theme choice not remembered");
        await page.evaluate(() => localStorage.removeItem("trove-theme"));
        await page.reload({ waitUntil: "networkidle" });
      }

      // Accessibility (WCAG 2.x A/AA rules).
      await page.addScriptTag({ content: axeSource });
      const violations = await page.evaluate(async () => {
        const result = await window.axe.run(document, {
          runOnly: { type: "tag", values: ["wcag2a", "wcag2aa", "wcag21a", "wcag21aa"] },
        });
        return result.violations.map((v) => `${v.id} (${v.nodes.length}): ${v.nodes[0]?.target.join(" ")}`);
      });
      violations.forEach((v) => fail(where, `axe ${v}`));

      await page
        .screenshot({ path: join(outDir, `${target.lang}-${viewport.name}-${scheme}.png`), fullPage: true, timeout: 90_000 })
        .catch((error) => console.log(`  (capture skipped for ${where}: ${error.message.split("\n")[0]})`));
      await context.close();
      console.log(`checked ${where}`);
    }
  }
}

await browser.close();

if (failures.length) {
  console.log(`\n${failures.length} problem(s):`);
  for (const line of [...new Set(failures)]) console.log(`  ${line}`);
  process.exitCode = 1;
} else {
  console.log("\nAll checks passed.");
}
