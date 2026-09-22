// Theme toggle and the screenshot lightbox. The initial theme is set by an
// inline script in <head> so the page never flashes the wrong one.
(() => {
  const root = document.documentElement;
  const toggle = document.querySelector(".theme-toggle");

  const applyTheme = (theme) => {
    root.dataset.theme = theme;
    toggle?.setAttribute("aria-pressed", String(theme === "dark"));
  };

  applyTheme(root.dataset.theme === "dark" ? "dark" : "light");

  toggle?.addEventListener("click", () => {
    const next = root.dataset.theme === "dark" ? "light" : "dark";
    applyTheme(next);
    try {
      localStorage.setItem("trove-theme", next);
    } catch {
      /* private mode: the choice lasts for this page only */
    }
  });

  // Follow the system while the visitor has not chosen.
  window.matchMedia("(prefers-color-scheme: dark)").addEventListener("change", (event) => {
    let stored = null;
    try {
      stored = localStorage.getItem("trove-theme");
    } catch {
      /* ignore */
    }
    if (!stored) applyTheme(event.matches ? "dark" : "light");
  });

  // Lightbox: every [data-lightbox] button opens the dialog on its picture.
  const dialog = document.querySelector(".lightbox");
  if (!dialog) return;
  const image = dialog.querySelector("img");
  const caption = dialog.querySelector("figcaption");
  const triggers = [...document.querySelectorAll("[data-lightbox]")];
  let current = 0;
  let opener = null;

  const show = (index) => {
    current = (index + triggers.length) % triggers.length;
    const trigger = triggers[current];
    const thumb = trigger.querySelector("img");
    image.src = trigger.dataset.lightbox;
    image.alt = thumb?.alt ?? "";
    image.width = Number(thumb?.getAttribute("width")) || 1040;
    image.height = Number(thumb?.getAttribute("height")) || 680;
    caption.textContent = trigger.dataset.caption ?? "";
  };

  triggers.forEach((trigger, index) => {
    trigger.addEventListener("click", () => {
      opener = trigger;
      show(index);
      dialog.showModal();
    });
  });

  dialog.querySelector("[data-prev]")?.addEventListener("click", () => show(current - 1));
  dialog.querySelector("[data-next]")?.addEventListener("click", () => show(current + 1));
  dialog.querySelector("[data-close]")?.addEventListener("click", () => dialog.close());

  dialog.addEventListener("keydown", (event) => {
    if (event.key === "ArrowLeft") show(current - 1);
    if (event.key === "ArrowRight") show(current + 1);
  });

  // A click on the backdrop (the dialog element itself) closes it.
  dialog.addEventListener("click", (event) => {
    if (event.target === dialog) dialog.close();
  });

  dialog.addEventListener("close", () => opener?.focus());
})();
