import { cleanup } from "@testing-library/preact";
import { afterEach, beforeEach, vi } from "vitest";
import { installDomStubs, resetDomStubs } from "./dom";
import { resetTauri } from "./tauri";

// ui.tsx decides once at import time whether IPC is available, so the marker has
// to exist before any component module is loaded.
(window as unknown as { __TAURI_INTERNALS__: unknown }).__TAURI_INTERNALS__ = {};

vi.mock("@tauri-apps/api/core", async () => await import("./tauri"));
vi.mock("@tauri-apps/api/event", async () => await import("./tauri"));
vi.mock("@tauri-apps/plugin-dialog", () => ({ open: vi.fn(async () => null) }));

installDomStubs();

beforeEach(() => {
  resetTauri();
  resetDomStubs();
});

afterEach(() => {
  cleanup();
});
