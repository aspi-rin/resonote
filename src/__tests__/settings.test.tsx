import { fireEvent, render, screen, waitFor, within } from "@testing-library/preact";
import { describe, expect, it, vi } from "vitest";
import { translator } from "../i18n";
import { ApiKeyField } from "../meeting_notes_ui";
import { appSettings, scriptAppDefaults } from "../test/fixtures";
import { openTab, renderApp, settingsSection, typeInto } from "../test/render";
import { callsOf, lastCall } from "../test/tauri";
import type { AppSettingsWithoutSecrets, SettingsSecretUpdates } from "../types";

const t = translator("en-US");
const LEAKED_KEY = "sk-stored-should-never-render";

function apiKeyInput(section: HTMLElement) {
  return within(section).getByPlaceholderText(t("apiKeyPlaceholder")) as HTMLInputElement;
}

function savedPayload() {
  const call = lastCall("save_settings");
  if (!call) throw new Error("save_settings was not called");
  return { secrets: call.args.secrets as SettingsSecretUpdates, settings: call.args.settings as AppSettingsWithoutSecrets };
}

describe("settings form", () => {
  it("reads and saves the translation and meeting-notes providers independently", async () => {
    scriptAppDefaults({
      settings: appSettings((next) => {
        next.meetingNotes.endpoint = "http://127.0.0.1:9000/v1";
        next.meetingNotes.model = "qwen-max";
        next.translation.endpoint = "http://127.0.0.1:8000/v1";
        next.translation.model = "Hy-MT2-1.8B";
      }),
    });
    await renderApp(t);
    openTab(t, "settings");

    expect((within(settingsSection(t("translation"))).getByLabelText(t("translationModel")) as HTMLInputElement).value).toBe("Hy-MT2-1.8B");
    expect((within(settingsSection(t("meetingNotesProvider"))).getByLabelText(t("meetingNotesModel")) as HTMLInputElement).value).toBe("qwen-max");

    typeInto(within(settingsSection(t("translation"))).getByLabelText(t("translationEndpoint")), "https://translate.example.com/v1");
    typeInto(within(settingsSection(t("meetingNotesProvider"))).getByLabelText(t("meetingNotesModel")), "gpt-5-mini");
    typeInto(apiKeyInput(settingsSection(t("translation"))), "translation-secret");
    typeInto(apiKeyInput(settingsSection(t("meetingNotesProvider"))), "notes-secret");
    fireEvent.click(screen.getByRole("button", { name: t("saveSettings") }));

    await waitFor(() => expect(callsOf("save_settings")).toHaveLength(1));
    const { secrets, settings } = savedPayload();
    expect(settings.translation.endpoint).toBe("https://translate.example.com/v1");
    expect(settings.translation.model).toBe("Hy-MT2-1.8B");
    expect(settings.meetingNotes.model).toBe("gpt-5-mini");
    expect(settings.meetingNotes.endpoint).toBe("http://127.0.0.1:9000/v1");
    expect(secrets).toEqual({
      meetingNotesApiKey: { action: "set", value: "notes-secret" },
      translationApiKey: { action: "set", value: "translation-secret" },
    });
  });

  it("never sends apiKeyConfigured back to the backend", async () => {
    scriptAppDefaults({ settings: appSettings((next) => { next.meetingNotes.apiKeyConfigured = true; next.translation.apiKeyConfigured = true; }) });
    await renderApp(t);
    openTab(t, "settings");
    fireEvent.click(screen.getByRole("button", { name: t("saveSettings") }));

    await waitFor(() => expect(callsOf("save_settings")).toHaveLength(1));
    expect(JSON.stringify(savedPayload().settings)).not.toContain("apiKeyConfigured");
  });

  it("keeps both stored keys when the key inputs are untouched", async () => {
    scriptAppDefaults();
    await renderApp(t);
    openTab(t, "settings");
    typeInto(within(settingsSection(t("meetingNotesProvider"))).getByLabelText(t("meetingNotesModel")), "gpt-5-mini");
    fireEvent.click(screen.getByRole("button", { name: t("saveSettings") }));

    await waitFor(() => expect(callsOf("save_settings")).toHaveLength(1));
    expect(savedPayload().secrets).toEqual({ meetingNotesApiKey: { action: "keep" }, translationApiKey: { action: "keep" } });
  });
});

describe("api key control", () => {
  it("marks a configured key without ever rendering its value", async () => {
    const settings = appSettings((next) => { next.meetingNotes.apiKeyConfigured = true; });
    (settings.meetingNotes as unknown as Record<string, unknown>).apiKey = LEAKED_KEY;
    scriptAppDefaults({ settings });
    await renderApp(t);
    openTab(t, "settings");

    const section = settingsSection(t("meetingNotesProvider"));
    expect(within(section).getByText(t("apiKeyConfigured"))).toBeTruthy();
    expect(within(settingsSection(t("translation"))).getByText(t("apiKeyMissing"))).toBeTruthy();
    expect(document.body.textContent).not.toContain(LEAKED_KEY);
    expect([...document.querySelectorAll("input")].map((input) => input.value)).not.toContain(LEAKED_KEY);
    expect(apiKeyInput(section).value).toBe("");
  });

  it("emits set while typing and keep once the input is emptied", () => {
    const onChange = vi.fn();
    render(<ApiKeyField configured={false} secret={{ action: "keep" }} t={t} onChange={onChange} />);
    const input = screen.getByPlaceholderText(t("apiKeyPlaceholder"));

    typeInto(input, "fresh-key");
    expect(onChange).toHaveBeenLastCalledWith({ action: "set", value: "fresh-key" });
    typeInto(input, "");
    expect(onChange).toHaveBeenLastCalledWith({ action: "keep" });
  });

  it("clears and un-clears a stored key", () => {
    const onChange = vi.fn();
    const view = render(<ApiKeyField configured={true} secret={{ action: "keep" }} t={t} onChange={onChange} />);
    fireEvent.click(screen.getByRole("button", { name: t("apiKeyClear") }));
    expect(onChange).toHaveBeenLastCalledWith({ action: "clear" });

    view.rerender(<ApiKeyField configured={true} secret={{ action: "clear" }} t={t} onChange={onChange} />);
    expect(screen.getByText(t("apiKeyWillClear"))).toBeTruthy();
    expect((screen.getByPlaceholderText(t("apiKeyPlaceholder")) as HTMLInputElement).disabled).toBe(true);
    fireEvent.click(screen.getByRole("button", { name: t("apiKeyUndoClear") }));
    expect(onChange).toHaveBeenLastCalledWith({ action: "keep" });
  });

  it("reveals only text the user typed", () => {
    const view = render(<ApiKeyField configured={true} secret={{ action: "keep" }} t={t} onChange={vi.fn()} />);
    expect((screen.getByRole("button", { name: t("apiKeyShow") }) as HTMLButtonElement).disabled).toBe(true);

    view.rerender(<ApiKeyField configured={true} secret={{ action: "set", value: "typed-key" }} t={t} onChange={vi.fn()} />);
    const input = screen.getByPlaceholderText(t("apiKeyPlaceholder")) as HTMLInputElement;
    expect(input.type).toBe("password");
    fireEvent.click(screen.getByRole("button", { name: t("apiKeyShow") }));
    expect(input.type).toBe("text");
    expect(input.value).toBe("typed-key");
  });
});

describe("insecure endpoint", () => {
  it("warns without blocking when no key is involved", async () => {
    scriptAppDefaults({ settings: appSettings((next) => { next.translation.endpoint = "http://notes.example.com/v1"; }) });
    await renderApp(t);
    openTab(t, "settings");

    expect(within(settingsSection(t("translation"))).getByText(t("insecureEndpointWarning"), { exact: false })).toBeTruthy();
    expect(screen.queryByText(t("insecureEndpointBlocked"))).toBeNull();
    expect((screen.getByRole("button", { name: t("saveSettings") }) as HTMLButtonElement).disabled).toBe(false);
  });

  it("blocks saving while a key would be sent over plain remote http", async () => {
    scriptAppDefaults({
      settings: appSettings((next) => {
        next.translation.apiKeyConfigured = true;
        next.translation.endpoint = "http://notes.example.com/v1";
      }),
    });
    await renderApp(t);
    openTab(t, "settings");

    expect(within(settingsSection(t("translation"))).getByRole("alert").textContent).toContain(t("insecureEndpointBlocked"));
    expect(screen.getByText(t("insecureEndpointBlocked"))).toBeTruthy();
    expect((screen.getByRole("button", { name: t("saveSettings") }) as HTMLButtonElement).disabled).toBe(true);

    fireEvent.click(within(settingsSection(t("translation"))).getByRole("button", { name: t("apiKeyClear") }));
    expect((screen.getByRole("button", { name: t("saveSettings") }) as HTMLButtonElement).disabled).toBe(false);
    expect(screen.queryByText(t("insecureEndpointBlocked"))).toBeNull();
  });

  it("blocks saving a freshly typed key for a plain remote endpoint", async () => {
    scriptAppDefaults({ settings: appSettings((next) => { next.meetingNotes.endpoint = "http://notes.example.com/v1"; }) });
    await renderApp(t);
    openTab(t, "settings");

    expect((screen.getByRole("button", { name: t("saveSettings") }) as HTMLButtonElement).disabled).toBe(false);
    typeInto(apiKeyInput(settingsSection(t("meetingNotesProvider"))), "notes-secret");
    expect((screen.getByRole("button", { name: t("saveSettings") }) as HTMLButtonElement).disabled).toBe(true);
    expect(callsOf("save_settings")).toHaveLength(0);
  });
});
