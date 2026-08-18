import { act, fireEvent, render, screen, waitFor, within } from "@testing-library/preact";
import { afterEach, describe, expect, it, vi } from "vitest";
import { translator } from "../i18n";
import { AUTOSAVE_DELAY_MS } from "../main";
import { ApiKeyField, apiKeySecret, secretBlocksSave } from "../meeting_notes_ui";
import { appSettings, errorPayload, scriptAppDefaults } from "../test/fixtures";
import { openTab, renderApp, settingsSection, typeInto } from "../test/render";
import { callsOf, lastCall, script, scriptFailure } from "../test/tauri";
import type { AppSettingsWithoutSecrets, ProviderKind, ProviderSettingsView, SettingsSecretUpdates, TestProviderSettingsRequest } from "../types";

const t = translator("en-US");
const STORED_KEY = "sk-stored-translation-key";

afterEach(() => { vi.useRealTimers(); });

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

/** The section-header pill, found by class because its label is the state. */
function testPill(section: HTMLElement) {
  const pill = section.querySelector<HTMLButtonElement>("button.provider-action-button");
  if (!pill) throw new Error("provider test pill not found");
  return pill;
}

/** Present whenever the field holds something to reveal. */
function revealButton(section: HTMLElement) {
  return section.querySelector<HTMLButtonElement>("button.api-key-reveal");
}

/** Settings with a stored translation key bound to the endpoint it is shown under. */
function withStoredKey(endpoint = "http://127.0.0.1:8000/v1") {
  return appSettings((next) => {
    next.translation.apiKey = STORED_KEY;
    next.translation.apiKeyConfigured = true;
    next.translation.endpoint = endpoint;
  });
}

/** Takes over the clock so the autosave debounce can be driven explicitly:
 *  waitFor cannot advance vitest's fake timers, so every later step is awaited
 *  through act instead. */
function freezeClock() {
  vi.useFakeTimers();
}

async function runDebounce(milliseconds = AUTOSAVE_DELAY_MS) {
  await act(async () => { vi.advanceTimersByTime(milliseconds); });
}

describe("settings autosave", () => {
  it("writes one save after a burst of edits and flashes the saved state", async () => {
    scriptAppDefaults({
      settings: appSettings((next) => {
        next.meetingNotes.endpoint = "http://127.0.0.1:9000/v1";
        next.meetingNotes.model = "qwen-max";
      }),
    });
    await renderApp(t);
    openTab(t, "settings");
    freezeClock();

    typeInto(within(settingsSection(t("translation"))).getByLabelText(t("translationEndpoint")), "https://translate.example.com/v1");
    typeInto(within(settingsSection(t("meetingNotesProvider"))).getByLabelText(t("meetingNotesModel")), "gpt-5-mini");
    await runDebounce(AUTOSAVE_DELAY_MS - 100);
    expect(callsOf("save_settings")).toHaveLength(0);

    await runDebounce();

    expect(callsOf("save_settings")).toHaveLength(1);
    const { secrets, settings } = savedPayload();
    expect(settings.translation.endpoint).toBe("https://translate.example.com/v1");
    expect(settings.translation.model).toBe("Hy-MT2-1.8B");
    expect(settings.meetingNotes.model).toBe("gpt-5-mini");
    expect(settings.meetingNotes.endpoint).toBe("http://127.0.0.1:9000/v1");
    expect(secrets).toEqual({ meetingNotesApiKey: { action: "keep" }, translationApiKey: { action: "keep" } });
    expect(JSON.stringify(settings)).not.toContain("apiKey");
    expect(JSON.stringify(settings)).not.toContain("verified");
    expect(screen.getByText(t("saved"))).toBeTruthy();
  });

  it("saves nothing while the form still matches the stored settings", async () => {
    scriptAppDefaults();
    await renderApp(t);
    openTab(t, "settings");
    freezeClock();

    await runDebounce();

    expect(callsOf("save_settings")).toHaveLength(0);
  });

  it("holds the save back while the key field has focus and flushes it on blur", async () => {
    scriptAppDefaults();
    await renderApp(t);
    openTab(t, "settings");
    freezeClock();
    const input = apiKeyInput(settingsSection(t("meetingNotesProvider")));

    fireEvent.focus(input);
    typeInto(input, "notes-secret");
    await runDebounce();
    expect(callsOf("save_settings")).toHaveLength(0);
    expect(apiKeyInput(settingsSection(t("meetingNotesProvider"))).value).toBe("notes-secret");

    await act(async () => { fireEvent.blur(input); });

    expect(callsOf("save_settings")).toHaveLength(1);
    expect(savedPayload().secrets.meetingNotesApiKey).toEqual({ action: "set", value: "notes-secret" });
    expect(within(settingsSection(t("meetingNotesProvider"))).getByText(t("apiKeyStored"))).toBeTruthy();
  });

  it("suspends the autosave while a key would travel to a remote host in the clear", async () => {
    scriptAppDefaults({ settings: appSettings((next) => { next.meetingNotes.endpoint = "http://notes.example.com/v1"; }) });
    await renderApp(t);
    openTab(t, "settings");
    freezeClock();

    typeInto(apiKeyInput(settingsSection(t("meetingNotesProvider"))), "notes-secret");
    await runDebounce();

    expect(callsOf("save_settings")).toHaveLength(0);
    expect(within(settingsSection(t("meetingNotesProvider"))).getByRole("alert").textContent).toContain(t("insecureEndpointBlocked"));
    expect(screen.getByText(t("insecureEndpointBlocked"))).toBeTruthy();

    typeInto(labelled(settingsSection(t("meetingNotesProvider")), t("meetingNotesEndpoint")), "https://notes.example.com/v1");
    await runDebounce();

    expect(callsOf("save_settings")).toHaveLength(1);
    expect(savedPayload().secrets.meetingNotesApiKey).toEqual({ action: "set", value: "notes-secret" });
    expect(screen.queryByText(t("insecureEndpointBlocked"))).toBeNull();
  });

  it("keeps the rejected form on screen and does not retry it", async () => {
    scriptAppDefaults();
    await renderApp(t);
    openTab(t, "settings");
    scriptFailure("save_settings", "invalid settings: meetingNotes endpoint must be a valid http or https URL");
    freezeClock();

    typeInto(within(settingsSection(t("meetingNotesProvider"))).getByLabelText(t("meetingNotesModel")), "gpt-5-mini");
    await runDebounce();

    expect(callsOf("save_settings")).toHaveLength(1);
    expect(screen.getByText("invalid settings: meetingNotes endpoint must be a valid http or https URL")).toBeTruthy();
    expect(labelled(settingsSection(t("meetingNotesProvider")), t("meetingNotesModel")).value).toBe("gpt-5-mini");

    await runDebounce();

    expect(callsOf("save_settings")).toHaveLength(1);
  });
});

describe("api key control", () => {
  it("shows the stored key and reveals it on demand", async () => {
    scriptAppDefaults({ settings: withStoredKey() });
    await renderApp(t);
    openTab(t, "settings");

    const section = settingsSection(t("translation"));
    expect(apiKeyInput(section).value).toBe(STORED_KEY);
    expect(apiKeyInput(section).type).toBe("password");
    expect(within(section).getByText(t("apiKeyStored"))).toBeTruthy();

    fireEvent.click(revealButton(section) as HTMLButtonElement);

    expect(apiKeyInput(settingsSection(t("translation"))).type).toBe("text");
    expect(apiKeyInput(settingsSection(t("translation"))).value).toBe(STORED_KEY);
    const empty = settingsSection(t("meetingNotesProvider"));
    expect(apiKeyInput(empty).value).toBe("");
    expect(revealButton(empty)).toBeNull();
    expect(within(empty).getByText(t("apiKeyNotStored"))).toBeTruthy();
  });

  it("clears a stored key once the field is emptied", async () => {
    scriptAppDefaults({ settings: withStoredKey() });
    await renderApp(t);
    openTab(t, "settings");
    freezeClock();

    typeInto(apiKeyInput(settingsSection(t("translation"))), "");
    expect(within(settingsSection(t("translation"))).getByText(t("apiKeyWillClear"))).toBeTruthy();
    await runDebounce();

    expect(savedPayload().secrets.translationApiKey).toEqual({ action: "clear" });
    const section = settingsSection(t("translation"));
    expect(apiKeyInput(section).value).toBe("");
    expect(within(section).getByText(t("apiKeyNotStored"))).toBeTruthy();
  });

  it("re-binds a visible key to the endpoint it is shown under", async () => {
    scriptAppDefaults({ settings: withStoredKey("https://first.example/v1") });
    await renderApp(t);
    openTab(t, "settings");
    freezeClock();

    typeInto(labelled(settingsSection(t("translation")), t("translationEndpoint")), "https://second.example/v1");
    await runDebounce();

    expect(savedPayload().secrets.translationApiKey).toEqual({ action: "set", value: STORED_KEY });
    expect(apiKeyInput(settingsSection(t("translation"))).value).toBe(STORED_KEY);
  });

  it("maps the field against the confirmed key", () => {
    const confirmed = (overrides: Partial<ProviderSettingsView> = {}): ProviderSettingsView => ({ apiKey: "", apiKeyConfigured: false, endpoint: "https://api.example.com/v1", model: "chat-model", verified: false, ...overrides });
    const stored = confirmed({ apiKey: "stored-key", apiKeyConfigured: true });

    expect(apiKeySecret("stored-key", "https://api.example.com/v1", stored)).toEqual({ action: "keep" });
    expect(apiKeySecret("stored-key", " https://api.example.com/v1/ ", stored)).toEqual({ action: "keep" });
    expect(apiKeySecret("rotated-key", "https://api.example.com/v1", stored)).toEqual({ action: "set", value: "rotated-key" });
    expect(apiKeySecret("stored-key", "https://other.example.com/v1", stored)).toEqual({ action: "set", value: "stored-key" });
    expect(apiKeySecret("  spaced-key  ", "https://api.example.com/v1", stored)).toEqual({ action: "set", value: "spaced-key" });
    expect(apiKeySecret("", "https://api.example.com/v1", stored)).toEqual({ action: "clear" });
    expect(apiKeySecret("   ", "https://api.example.com/v1", stored)).toEqual({ action: "clear" });
    expect(apiKeySecret("", "https://api.example.com/v1", confirmed())).toEqual({ action: "keep" });
    expect(apiKeySecret("fresh-key", "https://api.example.com/v1", confirmed())).toEqual({ action: "set", value: "fresh-key" });

    expect(secretBlocksSave("http://notes.example.com/v1", "stored-key")).toBe(true);
    expect(secretBlocksSave("http://notes.example.com/v1", "")).toBe(false);
    expect(secretBlocksSave("http://127.0.0.1:8000/v1", "stored-key")).toBe(false);
    expect(secretBlocksSave("https://notes.example.com/v1", "stored-key")).toBe(false);
  });

  it("reports every keystroke and both focus changes", () => {
    const onBlur = vi.fn();
    const onChange = vi.fn();
    const onFocus = vi.fn();
    render(<ApiKeyField configured={false} secret={{ action: "keep" }} t={t} value="" onBlur={onBlur} onChange={onChange} onFocus={onFocus} />);
    const input = screen.getByPlaceholderText(t("apiKeyPlaceholder"));

    typeInto(input, "fresh-key");
    expect(onChange).toHaveBeenLastCalledWith("fresh-key");
    fireEvent.focus(input);
    fireEvent.blur(input);
    expect(onFocus).toHaveBeenCalledTimes(1);
    expect(onBlur).toHaveBeenCalledTimes(1);
  });

  it("toggles the value between password and text", () => {
    render(<ApiKeyField configured={true} secret={{ action: "keep" }} t={t} value="typed-key" onChange={vi.fn()} />);
    expect((screen.getByPlaceholderText(t("apiKeyPlaceholder")) as HTMLInputElement).type).toBe("password");

    fireEvent.click(screen.getByRole("button", { name: t("apiKeyShow") }));
    const revealed = screen.getByPlaceholderText(t("apiKeyPlaceholder")) as HTMLInputElement;
    expect(revealed.type).toBe("text");
    expect(revealed.value).toBe("typed-key");

    fireEvent.click(screen.getByRole("button", { name: t("apiKeyHide") }));
    expect((screen.getByPlaceholderText(t("apiKeyPlaceholder")) as HTMLInputElement).type).toBe("password");
  });

  it("forgets the reveal when the value goes away", () => {
    const view = render(<ApiKeyField configured={true} secret={{ action: "keep" }} t={t} value="typed-key" onChange={vi.fn()} />);
    fireEvent.click(screen.getByRole("button", { name: t("apiKeyShow") }));
    expect((screen.getByPlaceholderText(t("apiKeyPlaceholder")) as HTMLInputElement).type).toBe("text");

    view.rerender(<ApiKeyField configured={true} secret={{ action: "clear" }} t={t} value="" onChange={vi.fn()} />);
    expect(screen.queryByRole("button", { name: t("apiKeyHide") })).toBeNull();
    expect(screen.getByText(t("apiKeyWillClear"))).toBeTruthy();

    view.rerender(<ApiKeyField configured={true} secret={{ action: "set", value: "another-key" }} t={t} value="another-key" onChange={vi.fn()} />);
    expect(screen.getByRole("button", { name: t("apiKeyShow") })).toBeTruthy();
    expect((screen.getByPlaceholderText(t("apiKeyPlaceholder")) as HTMLInputElement).type).toBe("password");
  });

  it("disables the input and the reveal button together", () => {
    render(<ApiKeyField configured={true} disabled={true} secret={{ action: "keep" }} t={t} value="typed-key" onChange={vi.fn()} />);

    expect((screen.getByPlaceholderText(t("apiKeyPlaceholder")) as HTMLInputElement).disabled).toBe(true);
    expect((screen.getByRole("button", { name: t("apiKeyShow") }) as HTMLButtonElement).disabled).toBe(true);
  });
});

describe("provider connection test", () => {
  it("saves the whole form, marks the provider verified and cancels the pending autosave", async () => {
    scriptAppDefaults();
    await renderApp(t);
    openTab(t, "settings");
    const section = settingsSection(t("meetingNotesProvider"));
    expect(testPill(section).textContent).toContain(t("providerTestConnection"));
    expect(testPill(section).disabled).toBe(false);
    freezeClock();

    typeInto(within(section).getByLabelText(t("meetingNotesModel")), "gpt-5-mini");
    typeInto(apiKeyInput(section), "notes-secret");
    await act(async () => { fireEvent.click(testPill(section)); });

    expect(callsOf("test_provider_settings")).toHaveLength(1);
    const request = testedPayload();
    expect(request.provider).toBe("meetingNotes");
    expect(request.settings.meetingNotes.model).toBe("gpt-5-mini");
    expect(request.secrets.meetingNotesApiKey).toEqual({ action: "set", value: "notes-secret" });
    expect(JSON.stringify(request.settings)).not.toContain("verified");
    expect(JSON.stringify(request.settings)).not.toContain("apiKey");
    const verified = settingsSection(t("meetingNotesProvider"));
    expect(testPill(verified).textContent).toContain(t("providerVerified"));
    expect(testPill(verified).disabled).toBe(true);
    expect(within(verified).queryByRole("alert")).toBeNull();
    expect(within(verified).getByText(t("apiKeyStored"))).toBeTruthy();
    expect(apiKeyInput(verified).value).toBe("notes-secret");
    expect(testPill(settingsSection(t("translation"))).textContent).toContain(t("providerTestConnection"));

    await runDebounce();

    expect(callsOf("save_settings")).toHaveLength(0);
  });

  it("reports a failed test without touching the stored settings", async () => {
    scriptAppDefaults({ settings: appSettings((next) => { next.translation.model = "Hy-MT2-1.8B"; }) });
    await renderApp(t);
    openTab(t, "settings");
    scriptFailure("test_provider_settings", errorPayload({ code: "PROVIDER_UNAUTHORIZED", messageKey: "meetingNotesErrorProviderUnauthorized", retryable: false }));

    fireEvent.click(testPill(settingsSection(t("translation"))));

    await screen.findByText(t("meetingNotesErrorProviderUnauthorized"));
    const section = settingsSection(t("translation"));
    expect(within(section).getByRole("alert").textContent).toBe(t("meetingNotesErrorProviderUnauthorized"));
    expect(testPill(section).textContent).toContain(t("providerTestFailedRetry"));
    expect(testPill(section).title).toBe(t("meetingNotesErrorProviderUnauthorized"));
    expect(testPill(section).disabled).toBe(false);
    expect((within(section).getByLabelText(t("translationModel")) as HTMLInputElement).value).toBe("Hy-MT2-1.8B");
    expect(callsOf("save_settings")).toHaveLength(0);

    fireEvent.click(testPill(section));
    await waitFor(() => expect(callsOf("test_provider_settings")).toHaveLength(2));
  });

  it("drops the verified badge as soon as the endpoint is edited", async () => {
    scriptAppDefaults({ settings: appSettings((next) => { next.translation.verified = true; }) });
    await renderApp(t);
    openTab(t, "settings");
    expect(testPill(settingsSection(t("translation"))).textContent).toContain(t("providerVerified"));

    typeInto(within(settingsSection(t("translation"))).getByLabelText(t("translationEndpoint")), "https://translate.example.com/v1");

    expect(testPill(settingsSection(t("translation"))).textContent).toContain(t("providerTestConnection"));
    expect(testPill(settingsSection(t("translation"))).disabled).toBe(false);
    expect(testPill(settingsSection(t("meetingNotesProvider"))).textContent).toContain(t("providerTestConnection"));
  });

  it("runs one request per click and locks the other buttons while it is in flight", async () => {
    scriptAppDefaults();
    await renderApp(t);
    openTab(t, "settings");
    fireEvent.click(testPill(settingsSection(t("translation"))));
    fireEvent.click(testPill(settingsSection(t("translation"))));

    expect(testPill(settingsSection(t("translation"))).textContent).toContain(t("providerTesting"));
    expect(testPill(settingsSection(t("translation"))).disabled).toBe(true);
    expect(testPill(settingsSection(t("meetingNotesProvider"))).disabled).toBe(true);
    expect(fetchButton(settingsSection(t("translation"))).disabled).toBe(true);
    await waitFor(() => expect(callsOf("test_provider_settings")).toHaveLength(1));
  });

  it("cannot test a provider whose key would travel over plain remote http", async () => {
    scriptAppDefaults({ settings: withStoredKey("http://notes.example.com/v1") });
    await renderApp(t);
    openTab(t, "settings");

    expect(testPill(settingsSection(t("translation"))).disabled).toBe(true);
    expect(testPill(settingsSection(t("meetingNotesProvider"))).disabled).toBe(false);

    typeInto(apiKeyInput(settingsSection(t("translation"))), "");
    expect(testPill(settingsSection(t("translation"))).disabled).toBe(false);
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
    expect(labelled(section, t("maxInputCharacters")).value).toBe("100000");
  });

  it("raises the character budget for the DeepSeek preset but leaves it untouched for presets without an override", async () => {
    scriptAppDefaults({
      settings: appSettings((next) => {
        next.meetingNotes.endpoint = "https://internal.example.com/v1";
        next.meetingNotes.maxInputCharacters = 48_000;
      }),
    });
    await renderApp(t);
    openTab(t, "settings");

    fireEvent.change(presetSelect(settingsSection(t("meetingNotesProvider"))), { target: { value: "deepseek" } });
    expect(labelled(settingsSection(t("meetingNotesProvider")), t("maxInputCharacters")).value).toBe("100000");

    // omlx ships no maxInputCharacters override, so the current value must survive the switch.
    fireEvent.change(presetSelect(settingsSection(t("meetingNotesProvider"))), { target: { value: "omlx" } });
    expect(labelled(settingsSection(t("meetingNotesProvider")), t("maxInputCharacters")).value).toBe("100000");
  });

  it("does not add a character budget field to the translation preset selection", async () => {
    scriptAppDefaults({
      settings: appSettings((next) => {
        next.translation.endpoint = "https://internal.example.com/v1";
      }),
    });
    await renderApp(t);
    openTab(t, "settings");

    expect(within(settingsSection(t("translation"))).queryByText(t("maxInputCharacters"))).toBeNull();
    fireEvent.change(presetSelect(settingsSection(t("translation"))), { target: { value: "deepseek" } });
    const section = settingsSection(t("translation"));
    expect(labelled(section, t("translationEndpoint")).value).toBe("https://api.deepseek.com/v1");
    expect(labelled(section, t("translationModel")).value).toBe("deepseek-chat");
    expect(within(section).queryByText(t("maxInputCharacters"))).toBeNull();
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
    freezeClock();
    typeInto(apiKeyInput(settingsSection(t("meetingNotesProvider"))), "notes-secret");

    fireEvent.click(fetchButton(settingsSection(t("meetingNotesProvider"))));
    expect(fetchButton(settingsSection(t("translation"))).disabled).toBe(true);
    expect(within(settingsSection(t("meetingNotesProvider"))).getByText(t("fetchingModels"))).toBeTruthy();
    await act(async () => {});

    expect(callsOf("list_provider_models")).toHaveLength(1);
    const request = fetchedPayload();
    expect(request.provider).toBe("meetingNotes");
    expect(request.settings.meetingNotes.endpoint).toBe("https://api.deepseek.com/v1");
    expect(request.secrets.meetingNotesApiKey).toEqual({ action: "set", value: "notes-secret" });
    expect(JSON.stringify(request.settings)).not.toContain("apiKey");
    expect(suggestions("meetingNotes")).toEqual(["deepseek-chat", "deepseek-reasoner", "deepseek-vl"]);
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
  });

  it("cannot fetch while a connection test is in flight or a key would travel in the clear", async () => {
    scriptAppDefaults({ settings: withStoredKey("http://notes.example.com/v1") });
    await renderApp(t);
    openTab(t, "settings");
    expect(fetchButton(settingsSection(t("translation"))).disabled).toBe(true);
    expect(fetchButton(settingsSection(t("meetingNotesProvider"))).disabled).toBe(false);

    fireEvent.click(testPill(settingsSection(t("meetingNotesProvider"))));

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
    freezeClock();

    expect(within(settingsSection(t("translation"))).getByText(t("insecureEndpointWarning"), { exact: false })).toBeTruthy();
    expect(screen.queryByText(t("insecureEndpointBlocked"))).toBeNull();

    typeInto(within(settingsSection(t("translation"))).getByLabelText(t("translationModel")), "other-model");
    await runDebounce();

    expect(callsOf("save_settings")).toHaveLength(1);
  });

  it("stops saving a stored key that the endpoint would expose", async () => {
    scriptAppDefaults({ settings: withStoredKey("http://notes.example.com/v1") });
    await renderApp(t);
    openTab(t, "settings");
    freezeClock();

    expect(within(settingsSection(t("translation"))).getByRole("alert").textContent).toContain(t("insecureEndpointBlocked"));
    expect(screen.getByText(t("insecureEndpointBlocked"))).toBeTruthy();
    typeInto(within(settingsSection(t("translation"))).getByLabelText(t("translationModel")), "other-model");
    await runDebounce();
    expect(callsOf("save_settings")).toHaveLength(0);

    typeInto(apiKeyInput(settingsSection(t("translation"))), "");
    await runDebounce();

    expect(screen.queryByText(t("insecureEndpointBlocked"))).toBeNull();
    expect(callsOf("save_settings")).toHaveLength(1);
    expect(savedPayload().secrets.translationApiKey).toEqual({ action: "clear" });
  });
});
