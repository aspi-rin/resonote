import { fireEvent, render, screen, waitFor, within } from "@testing-library/preact";
import { describe, expect, it, vi } from "vitest";
import { translator } from "../i18n";
import { ApiKeyField, KEY_SENTINEL } from "../meeting_notes_ui";
import { appSettings, errorPayload, scriptAppDefaults } from "../test/fixtures";
import { openTab, renderApp, settingsSection, typeInto } from "../test/render";
import { callsOf, lastCall, script, scriptFailure } from "../test/tauri";
import type { AppSettingsWithoutSecrets, ProviderKind, SettingsSecretUpdates, TestProviderSettingsRequest } from "../types";

const t = translator("en-US");
const LEAKED_KEY = "sk-stored-should-never-render";

function apiKeyInput(section: HTMLElement) {
  return within(section).getByPlaceholderText(t("apiKeyPlaceholder")) as HTMLInputElement;
}

function labelled(section: HTMLElement, label: string) {
  return within(section).getByLabelText(label) as HTMLInputElement;
}

function presetSelect(section: HTMLElement) {
  return within(section).getByLabelText(t("providerPreset")) as HTMLSelectElement;
}

function fetchButton(section: HTMLElement) {
  return within(section).getByRole("button", { name: t("fetchModels") }) as HTMLButtonElement;
}

function suggestions(provider: ProviderKind) {
  return [...document.querySelectorAll<HTMLOptionElement>(`#provider-models-${provider} option`)].map((option) => option.value);
}

function fetchedPayload() {
  const call = lastCall("list_provider_models");
  if (!call) throw new Error("list_provider_models was not called");
  return call.args.request as TestProviderSettingsRequest;
}

function savedPayload() {
  const call = lastCall("save_settings");
  if (!call) throw new Error("save_settings was not called");
  return { secrets: call.args.secrets as SettingsSecretUpdates, settings: call.args.settings as AppSettingsWithoutSecrets };
}

function testedPayload() {
  const call = lastCall("test_provider_settings");
  if (!call) throw new Error("test_provider_settings was not called");
  return call.args.request as TestProviderSettingsRequest;
}

function testButton(section: HTMLElement) {
  return within(section).getByRole("button", { name: t("providerTestConnection") }) as HTMLButtonElement;
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
  it("fills a configured field with the sentinel without ever rendering the stored value", async () => {
    const settings = appSettings((next) => { next.meetingNotes.apiKeyConfigured = true; });
    (settings.meetingNotes as unknown as Record<string, unknown>).apiKey = LEAKED_KEY;
    scriptAppDefaults({ settings });
    await renderApp(t);
    openTab(t, "settings");

    const section = settingsSection(t("meetingNotesProvider"));
    expect(within(section).getByText(t("apiKeyStored"))).toBeTruthy();
    expect(within(settingsSection(t("translation"))).getByText(t("apiKeyNotStored"))).toBeTruthy();
    expect(document.body.textContent).not.toContain(LEAKED_KEY);
    expect([...document.querySelectorAll("input")].map((input) => input.value)).not.toContain(LEAKED_KEY);
    expect(apiKeyInput(section).value).toBe(KEY_SENTINEL);
    expect(apiKeyInput(settingsSection(t("translation"))).value).toBe("");
  });

  it("emits set while typing and keep once an unconfigured input is emptied", () => {
    const onChange = vi.fn();
    render(<ApiKeyField configured={false} secret={{ action: "keep" }} t={t} onChange={onChange} />);
    const input = screen.getByPlaceholderText(t("apiKeyPlaceholder"));

    typeInto(input, "fresh-key");
    expect(onChange).toHaveBeenLastCalledWith({ action: "set", value: "fresh-key" });
    typeInto(input, "");
    expect(onChange).toHaveBeenLastCalledWith({ action: "keep" });
  });

  it("selects the sentinel on focus so one keystroke replaces it", () => {
    const onChange = vi.fn();
    render(<ApiKeyField configured={true} secret={{ action: "keep" }} t={t} onChange={onChange} />);
    const input = screen.getByPlaceholderText(t("apiKeyPlaceholder")) as HTMLInputElement;
    const select = vi.spyOn(input, "select");

    expect(input.value).toBe(KEY_SENTINEL);
    fireEvent.focus(input);
    expect(select).toHaveBeenCalledTimes(1);
    typeInto(input, "fresh-key");
    expect(onChange).toHaveBeenLastCalledWith({ action: "set", value: "fresh-key" });
  });

  it("strips the sentinel when the user types inside it", () => {
    const onChange = vi.fn();
    render(<ApiKeyField configured={true} secret={{ action: "keep" }} t={t} onChange={onChange} />);

    typeInto(screen.getByPlaceholderText(t("apiKeyPlaceholder")), "••••fresh-key••••");
    expect(onChange).toHaveBeenLastCalledWith({ action: "set", value: "fresh-key" });
    typeInto(screen.getByPlaceholderText(t("apiKeyPlaceholder")), KEY_SENTINEL);
    expect(onChange).toHaveBeenLastCalledWith({ action: "keep" });
  });

  it("clears a stored key when the field is emptied and stays editable afterwards", () => {
    const onChange = vi.fn();
    const view = render(<ApiKeyField configured={true} secret={{ action: "keep" }} t={t} onChange={onChange} />);
    typeInto(screen.getByPlaceholderText(t("apiKeyPlaceholder")), "");
    expect(onChange).toHaveBeenLastCalledWith({ action: "clear" });

    view.rerender(<ApiKeyField configured={true} secret={{ action: "clear" }} t={t} onChange={onChange} />);
    const input = screen.getByPlaceholderText(t("apiKeyPlaceholder")) as HTMLInputElement;
    expect(screen.getByText(t("apiKeyWillClear"))).toBeTruthy();
    expect(input.value).toBe("");
    expect(input.disabled).toBe(false);
    typeInto(input, "fresh-key");
    expect(onChange).toHaveBeenLastCalledWith({ action: "set", value: "fresh-key" });
  });

  it("toggles between password and text without revealing a stored key", () => {
    const view = render(<ApiKeyField configured={true} secret={{ action: "keep" }} t={t} onChange={vi.fn()} />);
    expect((screen.getByPlaceholderText(t("apiKeyPlaceholder")) as HTMLInputElement).type).toBe("password");

    fireEvent.click(screen.getByRole("button", { name: t("apiKeyShow") }));
    const revealed = screen.getByPlaceholderText(t("apiKeyPlaceholder")) as HTMLInputElement;
    expect(revealed.type).toBe("text");
    expect(revealed.value).toBe(KEY_SENTINEL);

    view.rerender(<ApiKeyField configured={true} secret={{ action: "set", value: "typed-key" }} t={t} onChange={vi.fn()} />);
    expect((screen.getByPlaceholderText(t("apiKeyPlaceholder")) as HTMLInputElement).value).toBe("typed-key");
    fireEvent.click(screen.getByRole("button", { name: t("apiKeyHide") }));
    expect((screen.getByPlaceholderText(t("apiKeyPlaceholder")) as HTMLInputElement).type).toBe("password");
  });

  it("disables the input and the reveal button together", () => {
    render(<ApiKeyField configured={true} disabled={true} secret={{ action: "keep" }} t={t} onChange={vi.fn()} />);

    expect((screen.getByPlaceholderText(t("apiKeyPlaceholder")) as HTMLInputElement).disabled).toBe(true);
    expect((screen.getByRole("button", { name: t("apiKeyShow") }) as HTMLButtonElement).disabled).toBe(true);
  });
});

describe("provider connection test", () => {
  it("saves the whole form and marks the tested provider verified", async () => {
    scriptAppDefaults();
    await renderApp(t);
    openTab(t, "settings");
    const section = settingsSection(t("meetingNotesProvider"));
    expect(within(section).getByText(t("providerUnverified"))).toBeTruthy();

    typeInto(within(section).getByLabelText(t("meetingNotesModel")), "gpt-5-mini");
    typeInto(apiKeyInput(section), "notes-secret");
    fireEvent.click(testButton(section));

    await waitFor(() => expect(callsOf("test_provider_settings")).toHaveLength(1));
    const request = testedPayload();
    expect(request.provider).toBe("meetingNotes");
    expect(request.settings.meetingNotes.model).toBe("gpt-5-mini");
    expect(request.secrets.meetingNotesApiKey).toEqual({ action: "set", value: "notes-secret" });
    expect(JSON.stringify(request.settings)).not.toContain("verified");
    expect(JSON.stringify(request.settings)).not.toContain("apiKeyConfigured");
    await screen.findByText(t("providerTestPassed"));
    expect(within(settingsSection(t("meetingNotesProvider"))).getByText(t("apiKeyStored"))).toBeTruthy();
    expect(apiKeyInput(settingsSection(t("meetingNotesProvider"))).value).toBe(KEY_SENTINEL);
    expect(within(settingsSection(t("translation"))).getByText(t("providerUnverified"))).toBeTruthy();

    fireEvent.click(screen.getByRole("button", { name: t("saved") }));
    await waitFor(() => expect(callsOf("save_settings")).toHaveLength(1));
    expect(savedPayload().secrets).toEqual({ meetingNotesApiKey: { action: "keep" }, translationApiKey: { action: "keep" } });
  });

  it("reports a failed test without touching the stored settings", async () => {
    scriptAppDefaults({ settings: appSettings((next) => { next.translation.model = "Hy-MT2-1.8B"; }) });
    await renderApp(t);
    openTab(t, "settings");
    scriptFailure("test_provider_settings", errorPayload({ code: "PROVIDER_UNAUTHORIZED", messageKey: "meetingNotesErrorProviderUnauthorized", retryable: false }));

    fireEvent.click(testButton(settingsSection(t("translation"))));

    await screen.findByText(t("meetingNotesErrorProviderUnauthorized"));
    const section = settingsSection(t("translation"));
    expect(within(section).getByRole("alert").textContent).toBe(t("meetingNotesErrorProviderUnauthorized"));
    expect(within(section).queryByText(t("providerVerified"))).toBeNull();
    expect((within(section).getByLabelText(t("translationModel")) as HTMLInputElement).value).toBe("Hy-MT2-1.8B");
    expect(callsOf("save_settings")).toHaveLength(0);
  });

  it("drops the verified badge as soon as the endpoint is edited", async () => {
    scriptAppDefaults({ settings: appSettings((next) => { next.translation.verified = true; }) });
    await renderApp(t);
    openTab(t, "settings");
    expect(within(settingsSection(t("translation"))).getByText(t("providerVerified"))).toBeTruthy();

    typeInto(within(settingsSection(t("translation"))).getByLabelText(t("translationEndpoint")), "https://translate.example.com/v1");

    expect(within(settingsSection(t("translation"))).getByText(t("providerUnverified"))).toBeTruthy();
    expect(within(settingsSection(t("meetingNotesProvider"))).getByText(t("providerUnverified"))).toBeTruthy();
  });

  it("runs one request per click and locks the other buttons while it is in flight", async () => {
    scriptAppDefaults();
    await renderApp(t);
    openTab(t, "settings");
    const button = testButton(settingsSection(t("translation")));

    fireEvent.click(button);
    fireEvent.click(button);

    expect(button.disabled).toBe(true);
    expect(testButton(settingsSection(t("meetingNotesProvider"))).disabled).toBe(true);
    expect((screen.getByRole("button", { name: t("saveSettings") }) as HTMLButtonElement).disabled).toBe(true);
    await waitFor(() => expect(callsOf("test_provider_settings")).toHaveLength(1));
  });

  it("cannot test a provider whose key would travel over plain remote http", async () => {
    scriptAppDefaults({ settings: appSettings((next) => { next.translation.apiKeyConfigured = true; next.translation.endpoint = "http://notes.example.com/v1"; }) });
    await renderApp(t);
    openTab(t, "settings");

    expect(testButton(settingsSection(t("translation"))).disabled).toBe(true);
    expect(testButton(settingsSection(t("meetingNotesProvider"))).disabled).toBe(false);

    typeInto(apiKeyInput(settingsSection(t("translation"))), "");
    expect(testButton(settingsSection(t("translation"))).disabled).toBe(false);
  });
});

describe("provider presets", () => {
  it("derives the selection from the endpoint and tolerates a trailing slash", async () => {
    scriptAppDefaults({
      settings: appSettings((next) => {
        next.meetingNotes.endpoint = "https://api.deepseek.com/v1/";
        next.translation.endpoint = "https://translate.example.com/v1";
      }),
    });
    await renderApp(t);
    openTab(t, "settings");

    expect(presetSelect(settingsSection(t("meetingNotesProvider"))).value).toBe("deepseek");
    expect(presetSelect(settingsSection(t("translation"))).value).toBe("custom");
    expect(within(settingsSection(t("translation"))).getByRole("option", { name: t("providerCustom") })).toBeTruthy();
    expect(labelled(settingsSection(t("meetingNotesProvider")), t("meetingNotesEndpoint")).readOnly).toBe(true);
    expect(labelled(settingsSection(t("translation")), t("translationEndpoint")).readOnly).toBe(false);
  });

  it("fills the endpoint and model from a preset and locks the endpoint", async () => {
    scriptAppDefaults({
      settings: appSettings((next) => {
        next.meetingNotes.endpoint = "https://internal.example.com/v1";
        next.meetingNotes.model = "internal-model";
      }),
    });
    await renderApp(t);
    openTab(t, "settings");
    expect(labelled(settingsSection(t("meetingNotesProvider")), t("meetingNotesEndpoint")).readOnly).toBe(false);

    fireEvent.change(presetSelect(settingsSection(t("meetingNotesProvider"))), { target: { value: "deepseek" } });

    const section = settingsSection(t("meetingNotesProvider"));
    expect(labelled(section, t("meetingNotesEndpoint")).value).toBe("https://api.deepseek.com/v1");
    expect(labelled(section, t("meetingNotesEndpoint")).readOnly).toBe(true);
    expect(labelled(section, t("meetingNotesModel")).value).toBe("deepseek-chat");
    expect(suggestions("meetingNotes")).toEqual(["deepseek-chat", "deepseek-reasoner"]);
  });

  it("clears the model for a preset that ships none and unlocks the endpoint on custom", async () => {
    scriptAppDefaults({ settings: appSettings((next) => { next.meetingNotes.endpoint = "https://api.deepseek.com/v1"; next.meetingNotes.model = "deepseek-chat"; }) });
    await renderApp(t);
    openTab(t, "settings");

    fireEvent.change(presetSelect(settingsSection(t("meetingNotesProvider"))), { target: { value: "omlx" } });
    expect(labelled(settingsSection(t("meetingNotesProvider")), t("meetingNotesEndpoint")).value).toBe("http://127.0.0.1:8000/v1");
    expect(labelled(settingsSection(t("meetingNotesProvider")), t("meetingNotesModel")).value).toBe("");
    expect(suggestions("meetingNotes")).toEqual([]);

    fireEvent.change(presetSelect(settingsSection(t("meetingNotesProvider"))), { target: { value: "custom" } });

    const section = settingsSection(t("meetingNotesProvider"));
    expect(presetSelect(section).value).toBe("custom");
    expect(labelled(section, t("meetingNotesEndpoint")).readOnly).toBe(false);
    expect(labelled(section, t("meetingNotesEndpoint")).value).toBe("http://127.0.0.1:8000/v1");
    typeInto(labelled(section, t("meetingNotesEndpoint")), "https://internal.example.com/v1");
    expect(labelled(settingsSection(t("meetingNotesProvider")), t("meetingNotesEndpoint")).value).toBe("https://internal.example.com/v1");
  });
});

describe("model discovery", () => {
  it("merges the fetched ids into the suggestions without saving anything", async () => {
    scriptAppDefaults({ settings: appSettings((next) => { next.meetingNotes.endpoint = "https://api.deepseek.com/v1"; next.meetingNotes.model = "deepseek-chat"; }) });
    script("list_provider_models", () => ["deepseek-vl", "deepseek-reasoner"]);
    await renderApp(t);
    openTab(t, "settings");
    typeInto(apiKeyInput(settingsSection(t("meetingNotesProvider"))), "notes-secret");

    fireEvent.click(fetchButton(settingsSection(t("meetingNotesProvider"))));
    expect(fetchButton(settingsSection(t("translation"))).disabled).toBe(true);
    expect(within(settingsSection(t("meetingNotesProvider"))).getByText(t("fetchingModels"))).toBeTruthy();

    await waitFor(() => expect(callsOf("list_provider_models")).toHaveLength(1));
    const request = fetchedPayload();
    expect(request.provider).toBe("meetingNotes");
    expect(request.settings.meetingNotes.endpoint).toBe("https://api.deepseek.com/v1");
    expect(request.secrets.meetingNotesApiKey).toEqual({ action: "set", value: "notes-secret" });
    expect(JSON.stringify(request.settings)).not.toContain("apiKeyConfigured");
    await waitFor(() => expect(suggestions("meetingNotes")).toEqual(["deepseek-chat", "deepseek-reasoner", "deepseek-vl"]));
    expect(suggestions("translation")).toEqual([]);
    expect(callsOf("save_settings")).toHaveLength(0);
  });

  it("reports a failed fetch in the user's language and keeps the form untouched", async () => {
    scriptAppDefaults();
    await renderApp(t);
    openTab(t, "settings");
    scriptFailure("list_provider_models", errorPayload({ code: "PROVIDER_UNAVAILABLE", messageKey: "meetingNotesErrorProviderUnavailable", retryable: true }));
    typeInto(labelled(settingsSection(t("meetingNotesProvider")), t("meetingNotesModel")), "gpt-5-mini");

    fireEvent.click(fetchButton(settingsSection(t("meetingNotesProvider"))));

    await screen.findByText(t("meetingNotesErrorProviderUnavailable"));
    const section = settingsSection(t("meetingNotesProvider"));
    expect(labelled(section, t("meetingNotesModel")).value).toBe("gpt-5-mini");
    expect(apiKeyInput(section).value).toBe("");
    expect(fetchButton(section).disabled).toBe(false);
    expect(callsOf("save_settings")).toHaveLength(0);
  });

  it("cannot fetch while a connection test is in flight or a key would travel in the clear", async () => {
    scriptAppDefaults({ settings: appSettings((next) => { next.translation.apiKeyConfigured = true; next.translation.endpoint = "http://notes.example.com/v1"; }) });
    await renderApp(t);
    openTab(t, "settings");
    expect(fetchButton(settingsSection(t("translation"))).disabled).toBe(true);
    expect(fetchButton(settingsSection(t("meetingNotesProvider"))).disabled).toBe(false);

    fireEvent.click(testButton(settingsSection(t("meetingNotesProvider"))));

    expect(fetchButton(settingsSection(t("meetingNotesProvider"))).disabled).toBe(true);
    expect(callsOf("list_provider_models")).toHaveLength(0);
    await waitFor(() => expect(callsOf("test_provider_settings")).toHaveLength(1));
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

    typeInto(apiKeyInput(settingsSection(t("translation"))), "");
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
