import { invoke } from "@tauri-apps/api/core";
import type { ComponentChildren } from "preact";
import { useEffect, useRef, useState } from "preact/hooks";
import { format, type TranslationKey, type translator } from "./i18n";
import { Icon, isTauri, NumberField, SelectField, SettingsSection, TextField, formatClock, formatDuration } from "./ui";
import {
  EMPTY_MEETING_CONTEXT,
  type ActionItem,
  type AnalysisResult,
  type AnalysisState,
  type AppSettings,
  type BackgroundStatement,
  type CleanedSegment,
  type ContextEntity,
  type GenerateMode,
  type GlobalContextContent,
  type GlossaryEntry,
  type HistoryEntry,
  type MeetingContextContent,
  type MeetingNotesErrorCode,
  type MeetingNotesErrorPayload,
  type MeetingNotesStatusEvent,
  type MeetingSummary,
  type OutputLanguage,
  type ParticipantContext,
  type ProviderKind,
  type ProviderSettingsView,
  type RunError,
  type SecretUpdate,
  type SessionAnalysisView,
  type SourcedStatement,
  type StaleReason,
  type TranscriptDocument,
  type TranscriptSegment,
} from "./types";

export type Translate = ReturnType<typeof translator>;
export type DetailTab = "transcript" | "cleaned" | "summary" | "context";
export type ProviderTestState = "failed" | "testing" | "unverified" | "verified";

/** Everything the settings view needs to render and run a connection test. */
export interface ProviderTestProps {
  confirmed: AppSettings;
  errors: Record<ProviderKind, string | null>;
  running: ProviderKind | null;
  test: (provider: ProviderKind) => void;
}

/** A known endpoint and the models it is expected to serve. */
export interface ProviderPreset {
  defaultModel: string;
  endpoint: string;
  id: string;
  label: string;
  models: string[];
}

/** The fetched half of the model combobox: never persisted, so a stale list
 *  cannot outlive the session that fetched it. */
export interface ProviderFetchProps {
  errors: Record<ProviderKind, string | null>;
  fetch: (provider: ProviderKind) => void;
  models: Record<ProviderKind, string[]>;
  running: ProviderKind | null;
}

/** oMLX serves whatever the local machine has loaded, so it ships no static
 *  list and relies on the fetch button. */
export const PROVIDER_PRESETS: ProviderPreset[] = [
  { defaultModel: "", endpoint: "http://127.0.0.1:8000/v1", id: "omlx", label: "oMLX", models: [] },
  { defaultModel: "deepseek-chat", endpoint: "https://api.deepseek.com/v1", id: "deepseek", label: "DeepSeek", models: ["deepseek-chat", "deepseek-reasoner"] },
];

export const CUSTOM_PRESET_ID = "custom";

function normalizeEndpoint(endpoint: string) { return endpoint.trim().replace(/\/+$/, ""); }

export function matchPreset(endpoint: string): ProviderPreset | null {
  const normalized = normalizeEndpoint(endpoint);
  return PROVIDER_PRESETS.find((preset) => normalizeEndpoint(preset.endpoint) === normalized) ?? null;
}

export function modelOptions(endpoint: string, fetched: string[]) {
  return [...new Set([...(matchPreset(endpoint)?.models ?? []), ...fetched])].sort();
}

/** Ordering metadata shared by the status event and the read view. */
export interface AnalysisStatus {
  completedChunks: number;
  documentRevision: number;
  generation: number;
  state: AnalysisState;
  totalChunks: number;
  updatedAt: string;
}

export const CONTEXT_CHARACTER_LIMIT = 20_000;
const LOOPBACK_HOSTS = ["127.0.0.1", "localhost", "[::1]", "::1"];
const RUNNING_STATES: AnalysisState[] = ["queued", "cleaning", "summarizing"];

export function endpointHost(endpoint: string) {
  try {
    return new URL(endpoint).host;
  } catch {
    return "";
  }
}

export function isPlainRemoteEndpoint(endpoint: string) {
  try {
    const url = new URL(endpoint);
    return url.protocol === "http:" && !LOOPBACK_HOSTS.includes(url.hostname);
  } catch {
    return false;
  }
}

/** The one combination the backend refuses: a key that would travel to a remote
 *  host in the clear. */
export function secretBlocksSave(endpoint: string, apiKey: string) {
  return isPlainRemoteEndpoint(endpoint) && apiKey.trim().length > 0;
}

export function contextCharacterCount(content: GlobalContextContent | MeetingContextContent) {
  return [...JSON.stringify(content)].length;
}

export function parseMeetingNotesError(reason: unknown): MeetingNotesErrorPayload | null {
  let value: unknown = reason;
  if (typeof reason === "string") {
    try {
      value = JSON.parse(reason);
    } catch {
      return null;
    }
  }
  if (!value || typeof value !== "object") return null;
  const payload = value as Partial<MeetingNotesErrorPayload>;
  if (typeof payload.code !== "string" || typeof payload.messageKey !== "string") return null;
  return { code: payload.code, messageKey: payload.messageKey, params: payload.params ?? {}, retryable: payload.retryable === true };
}

function localizedMessage(t: Translate, messageKey: string, params: Record<string, number>) {
  const template = t(messageKey as TranslationKey) as string | undefined;
  return template ? format(template, params) : null;
}

export function describeError(t: Translate, reason: unknown) {
  const payload = parseMeetingNotesError(reason);
  if (!payload) return String(reason);
  return localizedMessage(t, payload.messageKey, payload.params) ?? payload.code;
}

export function describeRunError(t: Translate, error: RunError) {
  return localizedMessage(t, error.messageKey, {}) ?? error.code;
}

export function analysisStatusFromEvent(event: MeetingNotesStatusEvent): AnalysisStatus {
  return {
    completedChunks: event.completedChunks,
    documentRevision: event.documentRevision,
    generation: event.generation,
    state: event.state,
    totalChunks: event.totalChunks,
    updatedAt: event.updatedAt,
  };
}

export function analysisStatusFromView(view: SessionAnalysisView): AnalysisStatus {
  const run = view.currentRun;
  return {
    completedChunks: run?.progress.completedChunks ?? 0,
    documentRevision: view.documentRevision,
    generation: run?.generation ?? 0,
    state: run?.state ?? (view.lastSuccessfulResult ? "complete" : "draft"),
    totalChunks: run?.progress.totalChunks ?? 0,
    updatedAt: run?.updatedAt ?? "",
  };
}

// A snapshot supersedes another one for the same session when it reflects a
// later document write, a later run, or a later update of the same run.
export function isNewerAnalysisStatus(candidate: AnalysisStatus, existing: AnalysisStatus) {
  if (candidate.documentRevision !== existing.documentRevision) return candidate.documentRevision > existing.documentRevision;
  if (candidate.generation !== existing.generation) return candidate.generation > existing.generation;
  return candidate.updatedAt >= existing.updatedAt;
}

export function analysisChipKey(state: AnalysisState): TranslationKey {
  if (state === "complete") return "meetingNotesComplete";
  if (state === "cancelled") return "meetingNotesCancelled";
  if (state === "failed") return "meetingNotesFailed";
  if (state === "draft") return "meetingNotesNotGenerated";
  return "meetingNotesRunning";
}

export function staleReasonKey(reason: StaleReason): TranslationKey {
  const keys: Record<StaleReason, TranslationKey> = {
    globalContextChanged: "staleGlobalContextChanged",
    meetingContextChanged: "staleMeetingContextChanged",
    outputLanguageChanged: "staleOutputLanguageChanged",
    pipelineChanged: "stalePipelineChanged",
    providerChanged: "staleProviderChanged",
    transcriptChanged: "staleTranscriptChanged",
  };
  return keys[reason];
}

export function transcriptCounts(transcript: TranscriptDocument | null) {
  const segments = transcript?.segments ?? [];
  return {
    complete: segments.filter((segment) => segment.status === "complete" && segment.text.trim().length > 0).length,
    failed: segments.filter((segment) => segment.status === "failed").length,
  };
}

export function generateReadiness(entry: HistoryEntry, transcript: TranscriptDocument | null): { ready: boolean; reason: TranslationKey | null } {
  if (entry.status === "recording") return { ready: false, reason: "notReadyRecording" };
  if (!entry.transcriptStatus) return { ready: false, reason: "notReadyEmpty" };
  if (entry.transcriptStatus === "pending" || entry.transcriptStatus === "processing") return { ready: false, reason: "notReadyTranscript" };
  if (transcript && transcriptCounts(transcript).complete === 0) return { ready: false, reason: "notReadyEmpty" };
  return { ready: true, reason: null };
}

export function needsPartialConfirmation(entry: HistoryEntry) {
  return entry.transcriptStatus === "partial" || entry.status === "interrupted" || entry.status === "failed";
}

export function cleanedPlainText(segments: CleanedSegment[]) {
  return segments.map((segment) => `[${formatClock(segment.startMs)}] ${segment.text}`).join("\n");
}

export function summaryPlainText(t: Translate, summary: MeetingSummary) {
  const lines = [summary.title, "", `${t("summaryOverview")}: ${summary.overview.text}`];
  const section = (label: string, items: string[]) => {
    lines.push("", label);
    lines.push(...(items.length === 0 ? [`- ${t("summaryEmptySection")}`] : items.map((item) => `- ${item}`)));
  };
  section(t("summaryBackground"), summary.background.map((item) => item.text));
  section(t("summaryKeyPoints"), summary.keyPoints.map((item) => item.text));
  section(t("summaryDecisions"), summary.decisions.map((item) => item.text));
  section(t("summaryActionItems"), summary.actionItems.map((item) => `${item.task} (${t("summaryOwner")}: ${item.owner ?? "—"}, ${t("summaryDueDate")}: ${item.dueDate ?? "—"})`));
  section(t("summaryOpenQuestions"), summary.openQuestions.map((item) => item.text));
  return lines.join("\n");
}

export function replaceAt<T>(values: T[], index: number, value: T) { return values.map((item, position) => (position === index ? value : item)); }
export function removeAt<T>(values: T[], index: number) { return values.filter((_, position) => position !== index); }
export function moveAt<T>(values: T[], index: number, offset: number) {
  const target = index + offset;
  if (target < 0 || target >= values.length) return values;
  const next = [...values];
  next[index] = values[target];
  next[target] = values[index];
  return next;
}
export function splitCommaList(text: string) { return text.split(/[,，]/).map((value) => value.trim()).filter((value) => value.length > 0); }

export const EMPTY_ENTITY: ContextEntity = { aliases: [], canonicalName: "", commonAsrErrors: [], description: "", id: "" };
export const EMPTY_GLOSSARY_ENTRY: GlossaryEntry = { aliases: [], commonAsrErrors: [], id: "", meaning: "", term: "" };
export const EMPTY_PARTICIPANT: ParticipantContext = { ...EMPTY_ENTITY, role: "", speakerLabel: null };

/** Turns the field value into the update the backend should receive, by diffing
 *  it against the last key the backend confirmed. A visible key belongs to the
 *  endpoint it is shown under, so moving the endpoint re-binds it; emptying the
 *  field is how a stored key is cleared. */
export function apiKeySecret(value: string, endpoint: string, confirmed: ProviderSettingsView): SecretUpdate {
  const key = value.trim();
  if (key === "") return confirmed.apiKey === "" ? { action: "keep" } : { action: "clear" };
  if (key === confirmed.apiKey && normalizeEndpoint(endpoint) === normalizeEndpoint(confirmed.endpoint)) return { action: "keep" };
  return { action: "set", value: key };
}

/** The API key state of both providers, resolved by the App so one diff drives
 *  the field, the save payload and the insecure-endpoint warning. */
export interface ProviderKeyProps {
  blocked: Record<ProviderKind, boolean>;
  focus: (focused: boolean) => void;
  secrets: Record<ProviderKind, SecretUpdate>;
  update: (provider: ProviderKind, value: string) => void;
  values: Record<ProviderKind, string>;
}

/** A plain text field: the settings view echoes the stored key back, so the eye
 *  reveals the credential the next request would really send. Focus is reported
 *  upwards because an autosave landing mid-edit would replace what is being
 *  typed with the backend's answer. */
export function ApiKeyField({ configured, disabled = false, onBlur, onChange, onFocus, secret, t, value }: { configured: boolean; disabled?: boolean; onBlur?: () => void; onChange: (value: string) => void; onFocus?: () => void; secret: SecretUpdate; t: Translate; value: string }) {
  const [revealed, setRevealed] = useState(false);
  const filled = value.length > 0;
  const chip = secret.action === "clear" ? "apiKeyWillClear" : configured ? "apiKeyStored" : "apiKeyNotStored";
  useEffect(() => { if (!filled) setRevealed(false); }, [filled]);
  return <div class="field field-wide api-key-field">
    <span>{t("apiKey")}</span>
    <div class={`api-key-row ${filled ? "has-reveal" : ""}`}>
      <input autocomplete="off" disabled={disabled} placeholder={t("apiKeyPlaceholder")} spellcheck={false} type={revealed ? "text" : "password"} value={value} onBlur={onBlur} onFocus={onFocus} onInput={(event) => onChange(event.currentTarget.value)} />
      {filled && <button aria-label={revealed ? t("apiKeyHide") : t("apiKeyShow")} class="api-key-reveal" disabled={disabled} type="button" onClick={() => setRevealed(!revealed)}><Icon name={revealed ? "eyeOff" : "eye"} /></button>}
    </div>
    <p class={`api-key-note ${chip}`}>{t(chip)}</p>
  </div>;
}

/** The wired field: one line at both call sites, one place for the focus and
 *  diff plumbing. */
export function ProviderKeyField({ configured, disabled = false, keys, provider, t }: { configured: boolean; disabled?: boolean; keys: ProviderKeyProps; provider: ProviderKind; t: Translate }) {
  return <ApiKeyField configured={configured} disabled={disabled} secret={keys.secrets[provider]} t={t} value={keys.values[provider]} onBlur={() => keys.focus(false)} onChange={(value) => keys.update(provider, value)} onFocus={() => keys.focus(true)} />;
}

/** The endpoint stays the source of truth — nothing new is persisted. Picking a
 *  preset rewrites the endpoint and the model together and locks the endpoint;
 *  picking Custom only unlocks it, leaving the current values alone. */
export function ProviderEndpointFields({ disabled = false, endpoint, endpointLabel, onEndpoint, onSelect, t }: { disabled?: boolean; endpoint: string; endpointLabel: string; onEndpoint: (endpoint: string) => void; onSelect: (preset: ProviderPreset) => void; t: Translate }) {
  const [custom, setCustom] = useState(false);
  const preset = custom ? null : matchPreset(endpoint);
  return <>
    <SelectField
      disabled={disabled}
      label={t("providerPreset")}
      options={[...PROVIDER_PRESETS.map((item) => ({ label: item.label, value: item.id })), { label: t("providerCustom"), value: CUSTOM_PRESET_ID }]}
      value={preset?.id ?? CUSTOM_PRESET_ID}
      onChange={(value) => { const chosen = PROVIDER_PRESETS.find((item) => item.id === value); setCustom(chosen === undefined); if (chosen) onSelect(chosen); }}
    />
    <TextField disabled={disabled} label={endpointLabel} readonly={preset !== null} type="url" value={endpoint} onChange={onEndpoint} />
  </>;
}

export function ProviderModelField({ className = "", disabled = false, endpoint, fetch, fetchBlocked, label, onChange, provider, t, value }: { className?: string; disabled?: boolean; endpoint: string; fetch: ProviderFetchProps; fetchBlocked: boolean; label: string; onChange: (value: string) => void; provider: ProviderKind; t: Translate; value: string }) {
  const listId = `provider-models-${provider}`;
  const fetching = fetch.running === provider;
  const error = fetch.errors[provider];
  const options = modelOptions(endpoint, fetch.models[provider]);
  return <div class={`field model-field ${className}`}>
    <span>{label}</span>
    <div class="model-row">
      <input aria-label={label} autocomplete="off" disabled={disabled} list={listId} spellcheck={false} value={value} onInput={(event) => onChange(event.currentTarget.value)} />
      <button aria-label={t("fetchModels")} class="icon-button model-refresh" disabled={disabled || fetchBlocked || fetch.running !== null} title={t("fetchModels")} type="button" onClick={() => fetch.fetch(provider)}><Icon name="refresh" /></button>
    </div>
    <datalist id={listId}>{options.map((option) => <option key={option} value={option} />)}</datalist>
    {(fetching || error !== null) && <p class={`model-note ${error !== null && !fetching ? "failed" : ""}`} role={error !== null && !fetching ? "alert" : undefined}>{fetching ? t("fetchingModels") : error}</p>}
  </div>;
}

export function EndpointWarning({ blocked, endpoint, t }: { blocked: boolean; endpoint: string; t: Translate }) {
  if (!isPlainRemoteEndpoint(endpoint)) return null;
  return <p class={`inline-warning ${blocked ? "blocking" : ""}`} role={blocked ? "alert" : undefined}>{t("insecureEndpointWarning")}{blocked ? ` ${t("insecureEndpointBlocked")}` : ""}</p>;
}

/** An edit the backend has not confirmed yet: only the fields the connection
 *  test exercises count, so a language or budget change stays verified. */
export function providerDirty(settings: AppSettings, confirmed: AppSettings, provider: ProviderKind, secret: SecretUpdate) {
  return secret.action !== "keep" || settings[provider].endpoint !== confirmed[provider].endpoint || settings[provider].model !== confirmed[provider].model;
}

export function providerTestState({ dirty, failed, testing, verified }: { dirty: boolean; failed: boolean; testing: boolean; verified: boolean }): ProviderTestState {
  if (testing) return "testing";
  if (verified && !dirty) return "verified";
  return failed ? "failed" : "unverified";
}

/** The section-header twin of the ASR model pill: it carries the whole state of
 *  the connection test, so the section body only has to explain a failure. */
export function ProviderTestButton({ disabled, error, state, t, test }: { disabled: boolean; error: string | null; state: ProviderTestState; t: Translate; test: () => void }) {
  const verified = state === "verified";
  const label = t(verified ? "providerVerified" : state === "testing" ? "providerTesting" : state === "failed" ? "providerTestFailedRetry" : "providerTestConnection");
  return <button aria-live="polite" class={`model-action-button provider-action-button ${state}`} disabled={disabled || verified || state === "testing"} title={state === "failed" && error ? error : label} type="button" onClick={test}>
    <span class="model-action-content">
      <span class="model-action-icon" aria-hidden="true">
        <span class={`model-action-icon-state ${verified ? "" : "active"}`}><Icon name="shield" /></span>
        <span class={`model-action-icon-state ready ${verified ? "active" : ""}`}><Icon name="check" /></span>
      </span>
      <span>{label}</span>
    </span>
  </button>;
}

/** Only the details the header pill cannot carry: why the last test failed. */
export function ProviderErrorNote({ error, state }: { error: string | null; state: ProviderTestState }) {
  if (state !== "failed" || !error) return null;
  return <p class="provider-status-note failed" role="alert"><i />{error}</p>;
}

export function MeetingNotesProviderSection({ action, blocked, fetch, fetchBlocked, keys, settings, status, t, update }: { action: ComponentChildren; blocked: boolean; fetch: ProviderFetchProps; fetchBlocked: boolean; keys: ProviderKeyProps; settings: AppSettings; status: ComponentChildren; t: Translate; update: (mutate: (next: AppSettings) => void) => void }) {
  return <SettingsSection action={action} icon="chat" title={t("meetingNotesProvider")}>
    <p class="section-note">{t("meetingNotesProviderHint")}</p>
    <div class="form-grid">
      <ProviderEndpointFields endpoint={settings.meetingNotes.endpoint} endpointLabel={t("meetingNotesEndpoint")} t={t} onEndpoint={(value) => update((next) => { next.meetingNotes.endpoint = value; })} onSelect={(preset) => update((next) => { next.meetingNotes.endpoint = preset.endpoint; next.meetingNotes.model = preset.defaultModel; })} />
      <ProviderModelField endpoint={settings.meetingNotes.endpoint} fetch={fetch} fetchBlocked={fetchBlocked} label={t("meetingNotesModel")} provider="meetingNotes" t={t} value={settings.meetingNotes.model} onChange={(value) => update((next) => { next.meetingNotes.model = value; })} />
      <ProviderKeyField configured={settings.meetingNotes.apiKeyConfigured} keys={keys} provider="meetingNotes" t={t} />
    </div>
    <EndpointWarning blocked={blocked} endpoint={settings.meetingNotes.endpoint} t={t} />
    {status}
    <details class="advanced-settings">
      <summary>{t("advancedSettings")}</summary>
      <div class="form-grid">
        <NumberField label={t("maxInputCharacters")} max={200_000} min={8_000} value={settings.meetingNotes.maxInputCharacters} onChange={(value) => update((next) => { next.meetingNotes.maxInputCharacters = value; })} />
        <NumberField label={t("requestTimeoutSeconds")} max={900} min={30} value={settings.meetingNotes.requestTimeoutSeconds} onChange={(value) => update((next) => { next.meetingNotes.requestTimeoutSeconds = value; })} />
      </div>
    </details>
  </SettingsSection>;
}

export function GlobalContextSection({ busy, content, locale, notice, onChange, revision, save, t }: { busy: boolean; content: GlobalContextContent; locale: OutputLanguage; notice: string | null; onChange: (content: GlobalContextContent) => void; revision: number; save: () => void; t: Translate }) {
  const characters = contextCharacterCount(content);
  return <SettingsSection icon="book" title={t("globalContext")} action={<span class="context-count">{format(t("contextCharacters"), { characters, limit: CONTEXT_CHARACTER_LIMIT })}</span>}>
    <p class="section-note">{t("globalContextHint")}</p>
    {notice && <p class="inline-warning" role="alert">{notice}</p>}
    <ContextEditor kind="global" t={t} value={content} onChange={onChange} />
    <div class="context-actions">
      <span class="context-revision">r{revision}</span>
      <ExtractContextPanel busy={busy} kind="global" locale={locale} t={t} value={content} onChange={onChange} />
      <button class="secondary-button accent" disabled={busy || characters > CONTEXT_CHARACTER_LIMIT} type="button" onClick={save}>{t("saveGlobalContext")}</button>
    </div>
  </SettingsSection>;
}

/** Additive by construction: a scalar the user already filled is never
 *  overwritten, a list only gains names it does not carry yet, and the free text
 *  grows by one block. Nothing here saves — the editor keeps the draft. */
export function mergeContextDraft<T extends GlobalContextContent | MeetingContextContent>(current: T, draft: T): T {
  return mergeRecord(current as unknown as ContextRecord, draft as unknown as ContextRecord) as unknown as T;
}

type ContextRecord = Record<string, unknown>;

function mergeRecord(current: ContextRecord, draft: ContextRecord): ContextRecord {
  const merged: ContextRecord = { ...current };
  for (const [key, value] of Object.entries(draft)) merged[key] = key === "freeText" ? appendBlock(String(current[key] ?? ""), String(value ?? "")) : mergeField(current[key], value);
  return merged;
}

function mergeField(current: unknown, draft: unknown): unknown {
  if (Array.isArray(current) && Array.isArray(draft)) return mergeList(current, draft);
  if (typeof current === "string") return current.trim() === "" ? draft : current;
  if (isContextRecord(current) && isContextRecord(draft)) return mergeRecord(current, draft);
  return current ?? draft;
}

function mergeList(current: unknown[], draft: unknown[]) {
  const seen = new Set(current.map(entryName));
  return [...current, ...draft.filter((entry) => { const name = entryName(entry); if (seen.has(name)) return false; seen.add(name); return true; })];
}

function entryName(entry: unknown) {
  if (typeof entry === "string") return entry.trim().toLowerCase();
  const record = isContextRecord(entry) ? entry : {};
  return String(record.canonicalName ?? record.term ?? "").trim().toLowerCase();
}

function appendBlock(current: string, draft: string) {
  const addition = draft.trim();
  if (addition === "") return current;
  return current.trim() === "" ? addition : `${current}\n\n${addition}`;
}

function isContextRecord(value: unknown): value is ContextRecord {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

export type ExtractContextPanelProps = ContextEditorProps & { busy: boolean; locale: OutputLanguage; openSettings?: () => void };

interface PickedFile { characters: number; name: string; text: string }

const MAX_TEXT_FILE_BYTES = 2 * 1024 * 1024;

export function joinDraftSources(text: string, files: PickedFile[]) {
  return files.reduce((joined, file) => `${joined}\n\n--- ${file.name} ---\n\n${file.text}`, text).trim();
}

/** Carries the editor props it feeds, so both context layers get the panel by
 *  handing it the value and setter they already own. */
export function ExtractContextPanel(props: ExtractContextPanelProps) {
  const { busy, locale, openSettings, t } = props;
  const [applied, setApplied] = useState(false);
  const [error, setError] = useState<{ code: MeetingNotesErrorCode | null; message: string } | null>(null);
  const [extracting, setExtracting] = useState(false);
  const [files, setFiles] = useState<PickedFile[]>([]);
  const [fileError, setFileError] = useState<string | null>(null);
  const [open, setOpen] = useState(false);
  const [text, setText] = useState("");
  const payload = joinDraftSources(text, files);

  const pick = (input: HTMLInputElement) => {
    setFileError(null);
    for (const file of Array.from(input.files ?? [])) {
      if (file.size > MAX_TEXT_FILE_BYTES) { setFileError(format(t("fileTooLarge"), { name: file.name })); continue; }
      const reader = new FileReader();
      reader.onload = () => { const content = String(reader.result ?? ""); setFiles((current) => [...current, { characters: [...content].length, name: file.name, text: content }]); };
      reader.readAsText(file, "utf-8");
    }
    input.value = "";
  };

  // The draft is merged into the editor, never saved: the user reviews it first.
  const extract = async () => {
    if (!isTauri || payload === "") return;
    setError(null);
    setExtracting(true);
    try {
      const request = { outputLanguage: locale, text: payload };
      if (props.kind === "global") props.onChange(mergeContextDraft(props.value, await invoke<GlobalContextContent>("extract_global_context_draft", { request })));
      else props.onChange(mergeContextDraft(props.value, await invoke<MeetingContextContent>("extract_meeting_context_draft", { request })));
      setApplied(true);
      setFiles([]);
      setOpen(false);
      setText("");
    } catch (reason) {
      setError({ code: parseMeetingNotesError(reason)?.code ?? null, message: describeError(t, reason) });
    } finally {
      setExtracting(false);
    }
  };

  return <>
    {applied && <span class="context-notice">{t("contextDraftApplied")}</span>}
    <button aria-expanded={open} class="secondary-button" disabled={busy} type="button" onClick={() => { setApplied(false); setOpen(!open); }}>{t("extractFromText")}</button>
    {open && <div class="confirm-panel extract-panel">
      <label class="field context-textarea"><span>{t("extractContextTitle")}</span><textarea placeholder={t("pasteTextPlaceholder")} rows={4} value={text} onInput={(event) => setText(event.currentTarget.value)} /></label>
      <label class="field extract-file-field"><span>{t("chooseTextFiles")}</span><input accept=".txt,.md,.markdown,text/plain" multiple type="file" onChange={(event) => pick(event.currentTarget)} /></label>
      {files.length > 0 && <ul class="extract-files">{files.map((file, index) => <li class="extract-file" key={`${file.name}-${index}`}>
        <span>{file.name}</span>
        <span class="context-count">{format(t("contextFileCharacters"), { characters: file.characters })}</span>
        <button class="row-button remove" title={t("listRemove")} type="button" onClick={() => setFiles(removeAt(files, index))}>×</button>
      </li>)}</ul>}
      {fileError && <p class="inline-warning" role="alert">{fileError}</p>}
      {extracting && <p class="extract-status">{t("extracting")}</p>}
      {error && <p class="run-error" role="alert">{error.message}</p>}
      <div class="confirm-actions">
        {error?.code === "MEETING_NOTES_NOT_CONFIGURED" && openSettings && <button class="secondary-button" type="button" onClick={openSettings}>{t("openMeetingNotesSettings")}</button>}
        <button class="secondary-button" type="button" onClick={() => setOpen(false)}>{t("cancel")}</button>
        <button class="secondary-button accent" disabled={busy || extracting || payload === ""} type="button" onClick={() => void extract()}>{t("extractContextRun")}</button>
      </div>
    </div>}
  </>;
}

export type ContextEditorProps =
  | { kind: "global"; onChange: (value: GlobalContextContent) => void; t: Translate; value: GlobalContextContent }
  | { kind: "meeting"; onChange: (value: MeetingContextContent) => void; t: Translate; value: MeetingContextContent };

export function ContextEditor(props: ContextEditorProps) {
  return <div class="context-editor">{props.kind === "global"
    ? <GlobalContextFields t={props.t} value={props.value} onChange={props.onChange} />
    : <MeetingContextFields t={props.t} value={props.value} onChange={props.onChange} />}</div>;
}

function GlobalContextFields({ onChange, t, value }: { onChange: (value: GlobalContextContent) => void; t: Translate; value: GlobalContextContent }) {
  const patch = (mutate: Partial<GlobalContextContent>) => onChange({ ...value, ...mutate });
  return <>
    <div class="context-field">
      <div class="context-field-head"><span>{t("contextIdentity")}</span></div>
      <p class="context-hint">{t("contextListHint")}</p>
      <div class="form-grid">
        <TextField label={t("contextCanonicalName")} placeholder={t("contextNamePlaceholder")} value={value.identity.canonicalName} onChange={(canonicalName) => patch({ identity: { ...value.identity, canonicalName } })} />
        <CommaListInput label={t("contextAliases")} placeholder={t("contextAliasesPlaceholder")} values={value.identity.aliases} onChange={(aliases) => patch({ identity: { ...value.identity, aliases } })} />
        <CommaListInput label={t("contextAsrErrors")} placeholder={t("contextAsrErrorsPlaceholder")} values={value.identity.commonAsrErrors} onChange={(commonAsrErrors) => patch({ identity: { ...value.identity, commonAsrErrors } })} />
        <TextField label={t("contextTimezone")} placeholder={t("contextTimezonePlaceholder")} value={value.timezone} onChange={(timezone) => patch({ timezone })} />
        <CommaListInput className="field-wide" label={t("contextPreferredLanguages")} placeholder={t("contextLanguagesPlaceholder")} values={value.preferredLanguages} onChange={(preferredLanguages) => patch({ preferredLanguages })} />
      </div>
    </div>
    <ListField label={t("contextRoles")} placeholder={t("contextRolesPlaceholder")} t={t} values={value.rolesAndAffiliations} onChange={(rolesAndAffiliations) => patch({ rolesAndAffiliations })} />
    <ContextTextArea label={t("contextKnowledge")} placeholder={t("contextKnowledgePlaceholder")} value={value.knowledgeBackground} onChange={(knowledgeBackground) => patch({ knowledgeBackground })} />
    <EntityListField label={t("contextRecurringPeople")} t={t} values={value.recurringPeople} onChange={(recurringPeople) => patch({ recurringPeople })} />
    <EntityListField label={t("contextRecurringOrganizations")} t={t} values={value.recurringOrganizations} onChange={(recurringOrganizations) => patch({ recurringOrganizations })} />
    <EntityListField label={t("contextRecurringProducts")} t={t} values={value.recurringProductsAndProjects} onChange={(recurringProductsAndProjects) => patch({ recurringProductsAndProjects })} />
    <GlossaryListField label={t("contextGlossary")} t={t} values={value.glossary} onChange={(glossary) => patch({ glossary })} />
    <ContextTextArea label={t("contextFreeText")} placeholder={t("contextFreeTextPlaceholder")} value={value.freeText} onChange={(freeText) => patch({ freeText })} />
  </>;
}

function MeetingContextFields({ onChange, t, value }: { onChange: (value: MeetingContextContent) => void; t: Translate; value: MeetingContextContent }) {
  const patch = (mutate: Partial<MeetingContextContent>) => onChange({ ...value, ...mutate });
  return <>
    <div class="form-grid">
      <TextField label={t("contextTitle")} placeholder={t("contextTitlePlaceholder")} value={value.title} onChange={(title) => patch({ title })} />
      <TextField label={t("contextDate")} placeholder={t("contextDatePlaceholder")} value={value.date} onChange={(date) => patch({ date })} />
      <TextField className="field-wide" label={t("contextPurpose")} placeholder={t("contextPurposePlaceholder")} value={value.purpose} onChange={(purpose) => patch({ purpose })} />
    </div>
    <ListField label={t("contextAgenda")} placeholder={t("contextAgendaPlaceholder")} t={t} values={value.agenda} onChange={(agenda) => patch({ agenda })} />
    <ParticipantListField label={t("contextParticipants")} t={t} values={value.participants} onChange={(participants) => patch({ participants })} />
    <ContextTextArea label={t("contextBackground")} placeholder={t("contextBackgroundPlaceholder")} value={value.background} onChange={(background) => patch({ background })} />
    <ListField label={t("contextPriorFacts")} placeholder={t("contextPriorFactsPlaceholder")} t={t} values={value.priorFactsAndDecisions} onChange={(priorFactsAndDecisions) => patch({ priorFactsAndDecisions })} />
    <GlossaryListField label={t("contextGlossary")} t={t} values={value.glossary} onChange={(glossary) => patch({ glossary })} />
    <ListField label={t("contextKeyNumbers")} placeholder={t("contextKeyNumbersPlaceholder")} t={t} values={value.keyNumbers} onChange={(keyNumbers) => patch({ keyNumbers })} />
    <ListField label={t("contextConstraints")} placeholder={t("contextConstraintsPlaceholder")} t={t} values={value.constraints} onChange={(constraints) => patch({ constraints })} />
    <ListField label={t("contextQuestions")} placeholder={t("contextQuestionsPlaceholder")} t={t} values={value.questionsToDiscuss} onChange={(questionsToDiscuss) => patch({ questionsToDiscuss })} />
    <ListField label={t("contextSourceNotes")} placeholder={t("contextSourceNotesPlaceholder")} t={t} values={value.sourceNotes} onChange={(sourceNotes) => patch({ sourceNotes })} />
    <ContextTextArea label={t("contextFreeText")} placeholder={t("contextFreeTextPlaceholder")} value={value.freeText} onChange={(freeText) => patch({ freeText })} />
  </>;
}

export function CommaListInput({ className = "", label, onChange, placeholder = "", values }: { className?: string; label: string; onChange: (values: string[]) => void; placeholder?: string; values: string[] }) {
  const joined = values.join(", ");
  const [text, setText] = useState(joined);
  useEffect(() => { setText((current) => (splitCommaList(current).join(", ") === joined ? current : joined)); }, [joined]);
  return <TextField className={className} label={label} placeholder={placeholder} value={text} onChange={(next) => { setText(next); onChange(splitCommaList(next)); }} />;
}

export function ContextTextArea({ label, onChange, placeholder = "", value }: { label: string; onChange: (value: string) => void; placeholder?: string; value: string }) {
  return <label class="field context-textarea"><span>{label}</span><textarea placeholder={placeholder} rows={3} value={value} onInput={(event) => onChange(event.currentTarget.value)} /></label>;
}

function RowActions<T>({ index, onChange, t, values }: { index: number; onChange: (values: T[]) => void; t: Translate; values: T[] }) {
  return <div class="row-actions">
    <button class="row-button" disabled={index === 0} title={t("listMoveUp")} type="button" onClick={() => onChange(moveAt(values, index, -1))}>↑</button>
    <button class="row-button" disabled={index === values.length - 1} title={t("listMoveDown")} type="button" onClick={() => onChange(moveAt(values, index, 1))}>↓</button>
    <button class="row-button remove" title={t("listRemove")} type="button" onClick={() => onChange(removeAt(values, index))}>×</button>
  </div>;
}

function ListHead({ add, label, t }: { add: () => void; label: string; t: Translate }) {
  return <div class="context-field-head"><span>{label}</span><button class="chip-button" type="button" onClick={add}>+ {t("listAdd")}</button></div>;
}

export function ListField({ label, onChange, placeholder = "", t, values }: { label: string; onChange: (values: string[]) => void; placeholder?: string; t: Translate; values: string[] }) {
  return <div class="context-field">
    <ListHead add={() => onChange([...values, ""])} label={label} t={t} />
    {values.length === 0
      ? <p class="context-empty">{t("contextEmptyList")}</p>
      : <ul class="context-list">{values.map((value, index) => <li class="context-row" key={index}>
        <input placeholder={placeholder} value={value} onInput={(event) => onChange(replaceAt(values, index, event.currentTarget.value))} />
        <RowActions index={index} t={t} values={values} onChange={onChange} />
      </li>)}</ul>}
  </div>;
}

export function EntityListField({ label, onChange, t, values }: { label: string; onChange: (values: ContextEntity[]) => void; t: Translate; values: ContextEntity[] }) {
  return <div class="context-field">
    <ListHead add={() => onChange([...values, { ...EMPTY_ENTITY }])} label={label} t={t} />
    <p class="context-hint">{t("contextListHint")}</p>
    {values.length === 0
      ? <p class="context-empty">{t("contextEmptyList")}</p>
      : <ul class="context-list">{values.map((entry, index) => {
        const patch = (mutate: Partial<ContextEntity>) => onChange(replaceAt(values, index, { ...entry, ...mutate }));
        return <li class="context-entry" key={entry.id || index}>
          <div class="form-grid">
            <TextField label={t("contextCanonicalName")} placeholder={t("contextNamePlaceholder")} value={entry.canonicalName} onChange={(canonicalName) => patch({ canonicalName })} />
            <CommaListInput label={t("contextAliases")} placeholder={t("contextAliasesPlaceholder")} values={entry.aliases} onChange={(aliases) => patch({ aliases })} />
            <CommaListInput label={t("contextAsrErrors")} placeholder={t("contextAsrErrorsPlaceholder")} values={entry.commonAsrErrors} onChange={(commonAsrErrors) => patch({ commonAsrErrors })} />
            <TextField label={t("contextDescription")} placeholder={t("contextDescriptionPlaceholder")} value={entry.description} onChange={(description) => patch({ description })} />
          </div>
          <RowActions index={index} t={t} values={values} onChange={onChange} />
        </li>;
      })}</ul>}
  </div>;
}

export function GlossaryListField({ label, onChange, t, values }: { label: string; onChange: (values: GlossaryEntry[]) => void; t: Translate; values: GlossaryEntry[] }) {
  return <div class="context-field">
    <ListHead add={() => onChange([...values, { ...EMPTY_GLOSSARY_ENTRY }])} label={label} t={t} />
    <p class="context-hint">{t("contextListHint")}</p>
    {values.length === 0
      ? <p class="context-empty">{t("contextEmptyList")}</p>
      : <ul class="context-list">{values.map((entry, index) => {
        const patch = (mutate: Partial<GlossaryEntry>) => onChange(replaceAt(values, index, { ...entry, ...mutate }));
        return <li class="context-entry" key={entry.id || index}>
          <div class="form-grid">
            <TextField label={t("contextTerm")} placeholder={t("contextTermPlaceholder")} value={entry.term} onChange={(term) => patch({ term })} />
            <CommaListInput label={t("contextAliases")} placeholder={t("contextAliasesPlaceholder")} values={entry.aliases} onChange={(aliases) => patch({ aliases })} />
            <CommaListInput label={t("contextAsrErrors")} placeholder={t("contextAsrErrorsPlaceholder")} values={entry.commonAsrErrors} onChange={(commonAsrErrors) => patch({ commonAsrErrors })} />
            <TextField label={t("contextMeaning")} placeholder={t("contextMeaningPlaceholder")} value={entry.meaning} onChange={(meaning) => patch({ meaning })} />
          </div>
          <RowActions index={index} t={t} values={values} onChange={onChange} />
        </li>;
      })}</ul>}
  </div>;
}

export function ParticipantListField({ label, onChange, t, values }: { label: string; onChange: (values: ParticipantContext[]) => void; t: Translate; values: ParticipantContext[] }) {
  return <div class="context-field">
    <ListHead add={() => onChange([...values, { ...EMPTY_PARTICIPANT }])} label={label} t={t} />
    <p class="context-hint">{t("contextListHint")}</p>
    {values.length === 0
      ? <p class="context-empty">{t("contextEmptyList")}</p>
      : <ul class="context-list">{values.map((entry, index) => {
        const patch = (mutate: Partial<ParticipantContext>) => onChange(replaceAt(values, index, { ...entry, ...mutate }));
        return <li class="context-entry" key={entry.id || index}>
          <div class="form-grid">
            <TextField label={t("contextCanonicalName")} placeholder={t("contextNamePlaceholder")} value={entry.canonicalName} onChange={(canonicalName) => patch({ canonicalName })} />
            <TextField label={t("contextRole")} placeholder={t("contextRolePlaceholder")} value={entry.role} onChange={(role) => patch({ role })} />
            <CommaListInput label={t("contextAliases")} placeholder={t("contextAliasesPlaceholder")} values={entry.aliases} onChange={(aliases) => patch({ aliases })} />
            <CommaListInput label={t("contextAsrErrors")} placeholder={t("contextAsrErrorsPlaceholder")} values={entry.commonAsrErrors} onChange={(commonAsrErrors) => patch({ commonAsrErrors })} />
            <TextField label={t("contextSpeakerLabel")} placeholder={t("contextSpeakerLabelPlaceholder")} value={entry.speakerLabel ?? ""} onChange={(speakerLabel) => patch({ speakerLabel: speakerLabel === "" ? null : speakerLabel })} />
            <TextField label={t("contextDescription")} placeholder={t("contextDescriptionPlaceholder")} value={entry.description} onChange={(description) => patch({ description })} />
          </div>
          <RowActions index={index} t={t} values={values} onChange={onChange} />
        </li>;
      })}</ul>}
  </div>;
}

function entitySummary(entry: ContextEntity) {
  const aliases = [...entry.aliases, ...entry.commonAsrErrors];
  return `${entry.canonicalName}${aliases.length > 0 ? ` (${aliases.join(", ")})` : ""}${entry.description ? ` — ${entry.description}` : ""}`;
}

function glossarySummary(entry: GlossaryEntry) {
  const aliases = [...entry.aliases, ...entry.commonAsrErrors];
  return `${entry.term}${aliases.length > 0 ? ` (${aliases.join(", ")})` : ""}${entry.meaning ? ` — ${entry.meaning}` : ""}`;
}

export function readonlyContextRows(t: Translate, kind: "global" | "meeting", content: GlobalContextContent | MeetingContextContent) {
  const rows: { label: string; value: string }[] = [];
  const push = (label: string, value: string | string[]) => {
    const text = Array.isArray(value) ? value.filter((item) => item.trim().length > 0).join("; ") : value.trim();
    if (text.length > 0) rows.push({ label, value: text });
  };
  if (kind === "global") {
    const global = content as GlobalContextContent;
    push(t("contextIdentity"), [global.identity.canonicalName, ...global.identity.aliases, ...global.identity.commonAsrErrors]);
    push(t("contextRoles"), global.rolesAndAffiliations);
    push(t("contextKnowledge"), global.knowledgeBackground);
    push(t("contextPreferredLanguages"), global.preferredLanguages);
    push(t("contextTimezone"), global.timezone);
    push(t("contextRecurringPeople"), global.recurringPeople.map(entitySummary));
    push(t("contextRecurringOrganizations"), global.recurringOrganizations.map(entitySummary));
    push(t("contextRecurringProducts"), global.recurringProductsAndProjects.map(entitySummary));
    push(t("contextGlossary"), global.glossary.map(glossarySummary));
    push(t("contextFreeText"), global.freeText);
    return rows;
  }
  const meeting = content as MeetingContextContent;
  push(t("contextTitle"), meeting.title);
  push(t("contextDate"), meeting.date);
  push(t("contextPurpose"), meeting.purpose);
  push(t("contextAgenda"), meeting.agenda);
  push(t("contextParticipants"), meeting.participants.map((entry) => `${entitySummary(entry)}${entry.role ? ` [${entry.role}]` : ""}${entry.speakerLabel ? ` [${entry.speakerLabel}]` : ""}`));
  push(t("contextBackground"), meeting.background);
  push(t("contextPriorFacts"), meeting.priorFactsAndDecisions);
  push(t("contextGlossary"), meeting.glossary.map(glossarySummary));
  push(t("contextKeyNumbers"), meeting.keyNumbers);
  push(t("contextConstraints"), meeting.constraints);
  push(t("contextQuestions"), meeting.questionsToDiscuss);
  push(t("contextSourceNotes"), meeting.sourceNotes);
  push(t("contextFreeText"), meeting.freeText);
  return rows;
}

export function ContextReadonly({ content, kind, t }: { content: GlobalContextContent | MeetingContextContent; kind: "global" | "meeting"; t: Translate }) {
  const rows = readonlyContextRows(t, kind, content);
  if (rows.length === 0) return <p class="context-empty">{t("contextEmptyReadonly")}</p>;
  return <dl class="context-readonly">{rows.map((row) => <div key={row.label}><dt>{row.label}</dt><dd>{row.value}</dd></div>)}</dl>;
}

export function MeetingNotesChips({ analysis, state, t }: { analysis: SessionAnalysisView | null; state: AnalysisState | null; t: Translate }) {
  if (!state) return null;
  return <>
    <span class={`mn-chip mn-${state}`}>{t("meetingNotes")}: {t(analysisChipKey(state))}</span>
    {analysis?.lastSuccessfulResult?.inputQuality.kind === "partial" && <span class="mn-chip partial">{t("meetingNotesPartialBadge")}</span>}
    {analysis?.freshness === "stale" && <span class="mn-chip stale">{t("meetingNotesStaleBadge")}</span>}
  </>;
}

export function SourceTimestamps({ cleaned, ids, onSelect }: { cleaned: CleanedSegment[]; ids: number[]; onSelect: (segmentId: number) => void }) {
  if (ids.length === 0) return null;
  return <span class="source-times">{ids.map((id) => {
    const segment = cleaned.find((item) => item.segmentId === id);
    return <button class="source-time" key={id} type="button" onClick={() => onSelect(id)}>{segment ? formatClock(segment.startMs) : `#${id}`}</button>;
  })}</span>;
}

function StatementList({ cleaned, items, onSelect, t }: { cleaned: CleanedSegment[]; items: SourcedStatement[]; onSelect: (segmentId: number) => void; t: Translate }) {
  if (items.length === 0) return <p class="summary-empty">{t("summaryEmptySection")}</p>;
  return <ul class="summary-list">{items.map((item, index) => <li key={index}><span>{item.text}</span><SourceTimestamps cleaned={cleaned} ids={item.sourceSegmentIds} onSelect={onSelect} /></li>)}</ul>;
}

function BackgroundList({ items, t }: { items: BackgroundStatement[]; t: Translate }) {
  if (items.length === 0) return <p class="summary-empty">{t("summaryEmptySection")}</p>;
  return <ul class="summary-list">{items.map((item, index) => <li key={index}><span>{item.text}</span><span class="context-paths">{item.contextPaths.map((path) => <code key={path}>{path}</code>)}</span></li>)}</ul>;
}

function ActionItemList({ cleaned, items, onSelect, t }: { cleaned: CleanedSegment[]; items: ActionItem[]; onSelect: (segmentId: number) => void; t: Translate }) {
  if (items.length === 0) return <p class="summary-empty">{t("summaryEmptySection")}</p>;
  return <ul class="summary-list action-items">{items.map((item, index) => <li key={index}>
    <span>{item.task}</span>
    <span class="action-meta">{t("summaryOwner")}: {item.owner ?? "—"} · {t("summaryDueDate")}: {item.dueDate ?? "—"}</span>
    <SourceTimestamps cleaned={cleaned} ids={item.sourceSegmentIds} onSelect={onSelect} />
  </li>)}</ul>;
}

export function SummaryPanel({ copy, copied, result, onSelect, t }: { copied: boolean; copy: () => void; onSelect: (segmentId: number) => void; result: AnalysisResult; t: Translate }) {
  const summary = result.summary;
  const cleaned = result.cleaned.segments;
  return <div class="summary-panel">
    <div class="detail-block-head"><h3>{summary.title}</h3><button class="icon-button" type="button" onClick={copy}><Icon name="copy" />{copied ? t("copied") : t("copyText")}</button></div>
    <section><h4>{t("summaryOverview")}</h4><p class="summary-overview">{summary.overview.text}<SourceTimestamps cleaned={cleaned} ids={summary.overview.sourceSegmentIds} onSelect={onSelect} /></p></section>
    <section><h4>{t("summaryBackground")}</h4><BackgroundList items={summary.background} t={t} /></section>
    <section><h4>{t("summaryKeyPoints")}</h4><StatementList cleaned={cleaned} items={summary.keyPoints} t={t} onSelect={onSelect} /></section>
    <section><h4>{t("summaryDecisions")}</h4><StatementList cleaned={cleaned} items={summary.decisions} t={t} onSelect={onSelect} /></section>
    <section><h4>{t("summaryActionItems")}</h4><ActionItemList cleaned={cleaned} items={summary.actionItems} t={t} onSelect={onSelect} /></section>
    <section><h4>{t("summaryOpenQuestions")}</h4><StatementList cleaned={cleaned} items={summary.openQuestions} t={t} onSelect={onSelect} /></section>
  </div>;
}

export function CleanedPanel({ copied, copy, highlight, segments, t }: { copied: boolean; copy: () => void; highlight: number | null; segments: CleanedSegment[]; t: Translate }) {
  const rows = useRef(new Map<number, HTMLLIElement>());
  useEffect(() => { if (highlight !== null) rows.current.get(highlight)?.scrollIntoView?.({ block: "center" }); }, [highlight]);
  if (segments.length === 0) return <p class="context-empty">{t("noCleanedText")}</p>;
  return <div class="cleaned-panel">
    <div class="detail-block-head"><h3>{t("tabCleaned")}</h3><button class="icon-button" type="button" onClick={copy}><Icon name="copy" />{copied ? t("copied") : t("copyText")}</button></div>
    <ol class="transcript-list">{segments.map((segment) => <li
      class={`transcript-row ${highlight === segment.segmentId ? "highlighted" : ""}`}
      key={segment.segmentId}
      ref={(element) => { if (element) rows.current.set(segment.segmentId, element); else rows.current.delete(segment.segmentId); }}
    ><time>{formatClock(segment.startMs)}</time><div class="transcript-copy"><span class="transcript-source">{segment.text}</span></div></li>)}</ol>
  </div>;
}

function RawTranscriptPanel({ segments, t }: { segments: TranscriptSegment[]; t: Translate }) {
  if (segments.length === 0) return <p class="context-empty">{t("noTranscript")}</p>;
  return <ol class="transcript-list">{segments.map((segment) => <li class={`transcript-row ${segment.status}`} key={segment.id}>
    <time>{formatDuration(segment.startMs)}</time>
    <div class="transcript-copy"><span class="transcript-source">{segment.status === "complete" ? segment.text.trim() || "…" : segment.status === "failed" ? t("transcriptFailed") : `${t("transcribing")}…`}</span></div>
  </li>)}</ol>;
}

function ResultMeta({ locale, result, stale, t }: { locale: string; result: AnalysisResult; stale: boolean; t: Translate }) {
  const provider = result.inputSnapshot.provider;
  return <div class="result-meta">
    <span>{t("resultGeneratedAt")}: {new Date(result.generatedAt).toLocaleString(locale)}</span>
    <span>{t("resultModel")}: {provider.model || "—"}</span>
    <span>{t("resultEndpointHost")}: {endpointHost(provider.endpoint) || "—"}</span>
    <span>{result.inputQuality.kind === "partial" ? t("resultInputPartial") : t("resultInputComplete")}</span>
    <span>{stale ? t("resultStale") : t("resultFresh")}</span>
  </div>;
}

export function MeetingNotesDetail({ analysis, busy, cancel, entry, error, generate, globalContext, locale, openSettings, retry, saveContext, settings, t }: {
  analysis: SessionAnalysisView | null;
  busy: boolean;
  cancel: () => void;
  entry: HistoryEntry;
  error: unknown;
  generate: (content: MeetingContextContent, options: { acceptPartial: boolean; mode: GenerateMode }) => void;
  globalContext: GlobalContextContent;
  locale: OutputLanguage;
  openSettings: () => void;
  retry: () => void;
  saveContext: (content: MeetingContextContent) => void;
  settings: AppSettings;
  t: Translate;
}) {
  const [tab, setTab] = useState<DetailTab | null>(null);
  const [transcript, setTranscript] = useState<TranscriptDocument | null>(null);
  const [draft, setDraft] = useState<MeetingContextContent>(analysis?.meetingContext.content ?? EMPTY_MEETING_CONTEXT);
  const [highlight, setHighlight] = useState<number | null>(null);
  const [confirming, setConfirming] = useState<GenerateMode | null>(null);
  const [copied, setCopied] = useState<DetailTab | null>(null);
  const revision = analysis?.meetingContext.revision ?? 0;
  const updatedAt = analysis?.meetingContext.updatedAt ?? "";

  useEffect(() => {
    setDraft(analysis?.meetingContext.content ?? EMPTY_MEETING_CONTEXT);
    // Re-seeding on a revision change keeps a save made elsewhere from being overwritten.
  }, [entry.sessionId, revision, updatedAt]);

  useEffect(() => {
    if (!isTauri) return;
    let disposed = false;
    void invoke<TranscriptDocument | null>("get_session_transcript", { sessionId: entry.sessionId })
      .then((document) => { if (!disposed) setTranscript(document); })
      .catch((reason) => console.warn("failed to load transcript", reason));
    return () => { disposed = true; };
  }, [entry.sessionId]);

  // The backend enforces the same barrier, so its answer reopens the dialog the
  // client-side readiness check did not predict.
  useEffect(() => { if (parseMeetingNotesError(error)?.code === "PARTIAL_CONFIRMATION_REQUIRED") setConfirming("ensure"); }, [error]);

  const result = analysis?.lastSuccessfulResult ?? null;
  const run = analysis?.currentRun ?? null;
  const activeTab = tab ?? (result ? "summary" : "transcript");
  const counts = transcriptCounts(transcript);

  const copy = (target: DetailTab, text: string) => {
    void navigator.clipboard?.writeText(text).then(() => {
      setCopied(target);
      window.setTimeout(() => setCopied(null), 1800);
    }).catch((reason) => console.warn("failed to copy", reason));
  };

  const selectSource = (segmentId: number) => { setTab("cleaned"); setHighlight(segmentId); };
  const start = (mode: GenerateMode) => {
    if (needsPartialConfirmation(entry)) setConfirming(mode);
    else generate(draft, { acceptPartial: false, mode });
  };
  const regenerate = () => { if (window.confirm(t("regenerateConfirm"))) start("regenerate"); };

  return <section class="detail-panel">
    <RunPanel
      busy={busy} cancel={cancel} entry={entry} error={error} generate={() => start("ensure")} locale={locale}
      openSettings={openSettings} regenerate={regenerate} result={result} retry={retry} run={run}
      settings={settings} staleReasons={analysis?.staleReasons ?? []} stale={analysis?.freshness === "stale"} t={t} transcript={transcript}
    />
    {confirming && <div class="confirm-panel" role="alertdialog">
      <strong>{t("partialConfirmTitle")}</strong>
      <p>{format(t("partialConfirmBody"), { complete: counts.complete, failed: counts.failed })}</p>
      <div class="confirm-actions">
        <button class="secondary-button" type="button" onClick={() => setConfirming(null)}>{t("cancel")}</button>
        <button class="secondary-button accent" disabled={busy} type="button" onClick={() => { setConfirming(null); generate(draft, { acceptPartial: true, mode: confirming }); }}>{t("partialConfirmAccept")}</button>
      </div>
    </div>}
    <div class="detail-tabs" role="tablist">
      {([["transcript", "tabTranscript"], ["cleaned", "tabCleaned"], ["summary", "tabSummary"], ["context", "tabContext"]] as const).map(([value, key]) =>
        <button aria-selected={activeTab === value} class={`detail-tab ${activeTab === value ? "active" : ""}`} key={value} role="tab" type="button" onClick={() => setTab(value)}>{t(key)}</button>)}
    </div>
    <div class="detail-body">
      {activeTab === "transcript" && <RawTranscriptPanel segments={transcript?.segments ?? []} t={t} />}
      {activeTab === "cleaned" && <CleanedPanel copied={copied === "cleaned"} highlight={highlight} segments={result?.cleaned.segments ?? []} t={t} copy={() => copy("cleaned", cleanedPlainText(result?.cleaned.segments ?? []))} />}
      {activeTab === "summary" && (result
        ? <SummaryPanel copied={copied === "summary"} result={result} t={t} copy={() => copy("summary", summaryPlainText(t, result.summary))} onSelect={selectSource} />
        : <p class="context-empty">{t("noSummary")}</p>)}
      {activeTab === "context" && <div class="context-panel">
        <div class="context-block">
          <div class="detail-block-head"><h3>{t("meetingContext")}</h3><span class="context-count">{format(t("contextCharacters"), { characters: contextCharacterCount(draft), limit: CONTEXT_CHARACTER_LIMIT })}</span></div>
          <p class="context-hint">{t("meetingContextHint")}</p>
          <ContextEditor kind="meeting" t={t} value={draft} onChange={setDraft} />
          <div class="context-actions">
            <span class="context-revision">r{revision}</span>
            <ExtractContextPanel busy={busy} kind="meeting" locale={locale} openSettings={openSettings} t={t} value={draft} onChange={setDraft} />
            <button class="secondary-button accent" disabled={busy} type="button" onClick={() => saveContext(draft)}>{t("saveMeetingContext")}</button>
          </div>
        </div>
        <div class="context-block">
          <div class="detail-block-head"><h3>{t("contextCurrentGlobal")}</h3></div>
          <ContextReadonly content={globalContext} kind="global" t={t} />
        </div>
        {result && <div class="context-block snapshot">
          <div class="detail-block-head"><h3>{t("contextSnapshot")}</h3></div>
          <p class="context-hint">{t("contextSnapshotHint")}</p>
          <h4>{t("globalContext")}</h4>
          <ContextReadonly content={result.inputSnapshot.context.global} kind="global" t={t} />
          <h4>{t("meetingContext")}</h4>
          <ContextReadonly content={result.inputSnapshot.context.meeting} kind="meeting" t={t} />
        </div>}
      </div>}
    </div>
  </section>;
}

function RunPanel({ busy, cancel, entry, error, generate, locale, openSettings, regenerate, result, retry, run, settings, stale, staleReasons, t, transcript }: {
  busy: boolean;
  cancel: () => void;
  entry: HistoryEntry;
  error: unknown;
  generate: () => void;
  locale: string;
  openSettings: () => void;
  regenerate: () => void;
  result: AnalysisResult | null;
  retry: () => void;
  run: SessionAnalysisView["currentRun"];
  settings: AppSettings;
  stale: boolean;
  staleReasons: StaleReason[];
  t: Translate;
  transcript: TranscriptDocument | null;
}) {
  const readiness = generateReadiness(entry, transcript);
  const active = run && RUNNING_STATES.includes(run.state) ? run : null;
  const host = endpointHost(settings.meetingNotes.endpoint) || t("meetingNotesEndpointUnset");
  const payload = parseMeetingNotesError(error);
  const notConfigured = payload?.code === "MEETING_NOTES_NOT_CONFIGURED" || run?.error?.code === "MEETING_NOTES_NOT_CONFIGURED";
  return <div class="run-panel">
    <div class="run-actions">
      {active
        ? <button class="secondary-button" disabled={busy || active.cancellationRequested} type="button" onClick={cancel}>{active.cancellationRequested ? t("stoppingRun") : t("stopRun")}</button>
        : run?.state === "cancelled"
          ? <>
            <button class="secondary-button accent" disabled={busy} type="button" onClick={retry}>{t("resumeRun")}</button>
            <button class="secondary-button" disabled={busy || !readiness.ready} type="button" onClick={regenerate}>{t("regenerate")}</button>
          </>
          : <button class="secondary-button accent" disabled={busy || !readiness.ready} type="button" onClick={result ? regenerate : generate}>{result ? t("regenerate") : t("generateMeetingNotes")}</button>}
      {run?.state === "failed" && run.error?.retryable && <button class="secondary-button" disabled={busy} type="button" onClick={retry}>{t("retryRun")}</button>}
      {notConfigured && <button class="secondary-button" type="button" onClick={openSettings}>{t("openMeetingNotesSettings")}</button>}
      {active && <span class="run-progress">{t(active.stage === "summarizing" ? "stageSummarizing" : active.stage === "cleaning" ? "stageCleaning" : "stageQueued")} · {format(t("progressChunks"), { completed: active.progress.completedChunks, total: active.progress.totalChunks })}</span>}
    </div>
    <p class="run-disclosure">{format(t("meetingNotesDisclosure"), { host })}</p>
    {!readiness.ready && readiness.reason && <p class="run-reason">{t(readiness.reason)}</p>}
    {run?.state === "failed" && run.error && <p class="run-error" role="alert">{describeRunError(t, run.error)}</p>}
    {error != null && <p class="run-error" role="alert">{describeError(t, error)}</p>}
    {result && <ResultMeta locale={locale} result={result} stale={stale} t={t} />}
    {stale && <p class="run-stale">{t("resultStale")}: {staleReasons.map((reason) => t(staleReasonKey(reason))).join("; ")}</p>}
  </div>;
}
