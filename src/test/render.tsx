import { fireEvent, render, screen, waitFor, within } from "@testing-library/preact";
import type { ComponentProps } from "preact";
import { expect, vi } from "vitest";
import { translator } from "../i18n";
import { App } from "../main";
import type { MeetingNotesDetail } from "../meeting_notes_ui";
import { analysisView, appSettings, globalContent, historyEntry } from "./fixtures";
import { listenerCount } from "./tauri";

export type Translate = ReturnType<typeof translator>;
export type DetailProps = ComponentProps<typeof MeetingNotesDetail>;

/** Props for a standalone MeetingNotesDetail; every callback is a spy. */
export function detailProps(overrides: Partial<DetailProps> = {}): DetailProps {
  return {
    analysis: analysisView(),
    busy: false,
    cancel: vi.fn(),
    entry: historyEntry(),
    error: null,
    generate: vi.fn(),
    globalContext: globalContent(),
    locale: "en-US",
    openSettings: vi.fn(),
    retry: vi.fn(),
    saveContext: vi.fn(),
    settings: appSettings(),
    t: translator("en-US"),
    ...overrides,
  };
}

/** Renders the real App and waits until the startup loads and the event
 *  subscriptions have settled. */
export async function renderApp(t: Translate = translator("en-US")) {
  const view = render(<App />);
  await screen.findByRole("button", { name: t("settings") });
  await waitFor(() => expect(listenerCount("meeting-notes-status")).toBe(1));
  return view;
}

export function openTab(t: Translate, tab: "record" | "history" | "settings") {
  fireEvent.click(screen.getByRole("button", { name: t(tab) }));
}

/** Expands the meeting-notes detail of the first history card. */
export async function expandSession(t: Translate) {
  fireEvent.click(screen.getAllByRole("button", { name: t("meetingNotes") })[0]);
  await screen.findByRole("tablist");
}

export function selectDetailTab(t: Translate, key: "tabTranscript" | "tabCleaned" | "tabSummary" | "tabContext") {
  fireEvent.click(within(screen.getByRole("tablist")).getByRole("tab", { name: t(key) }));
}

export function settingsSection(title: string): HTMLElement {
  const section = screen.getByRole("heading", { level: 2, name: title }).closest("section");
  if (!section) throw new Error(`settings section not found: ${title}`);
  return section as HTMLElement;
}

/** Scopes to the list/entity block whose head carries the given label. */
export function contextField(label: string): HTMLElement {
  const field = screen.getByText(label).closest(".context-field");
  if (!field) throw new Error(`context field not found: ${label}`);
  return field as HTMLElement;
}

export function typeInto(field: HTMLElement, value: string) {
  fireEvent.input(field, { target: { value } });
}
