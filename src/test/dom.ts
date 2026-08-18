import { vi } from "vitest";

/** jsdom has no clipboard, no confirm dialog and no layout, so the UI hooks into these. */
export const clipboardWrite = vi.fn(async (_text: string) => {});
export const confirmDialog = vi.fn(() => true);
export const scrollIntoView = vi.fn();

export const DESKTOP_VIEWPORT = { height: 800, width: 1280 };
export const MOBILE_VIEWPORT = { height: 667, width: 375 };

export function installDomStubs() {
  Element.prototype.scrollIntoView = scrollIntoView as unknown as Element["scrollIntoView"];
  window.confirm = confirmDialog as unknown as Window["confirm"];
  Object.defineProperty(navigator, "clipboard", { configurable: true, value: { writeText: clipboardWrite } });
}

export function setViewport({ height, width }: { height: number; width: number }) {
  Object.defineProperty(window, "innerHeight", { configurable: true, value: height, writable: true });
  Object.defineProperty(window, "innerWidth", { configurable: true, value: width, writable: true });
  window.dispatchEvent(new Event("resize"));
}

export function resetDomStubs() {
  clipboardWrite.mockClear();
  confirmDialog.mockClear();
  confirmDialog.mockReturnValue(true);
  scrollIntoView.mockClear();
  setViewport(DESKTOP_VIEWPORT);
}
