import { invoke } from "@tauri-apps/api/core";
import type { ComponentChildren } from "preact";
import { useEffect, useRef, useState } from "preact/hooks";
import { format, type TranslationKey, type translator } from "./i18n";
import { Icon, isTauri, NumberField, SettingsSection, TextField, formatClock, formatDuration } from "./ui";
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
  type MeetingNotesErrorPayload,
  type MeetingNotesStatusEvent,
  type MeetingSummary,
  type ParticipantContext,
  type ProviderKind,
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
  passed: ProviderKind | null;
  running: ProviderKind | null;
  test: (provider: ProviderKind) => void;
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

export function secretBlocksSave(endpoint: string, configured: boolean, secret: SecretUpdate) {
  if (!isPlainRemoteEndpoint(endpoint)) return false;
  if (secret.action === "set") return secret.value.trim().length > 0;
  if (secret.action === "clear") return false;
  return configured;
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

export function ApiKeyField({ configured, disabled = false, onChange, secret, t }: { configured: boolean; disabled?: boolean; onChange: (secret: SecretUpdate) => void; secret: SecretUpdate; t: Translate }) {
  const [revealed, setRevealed] = useState(false);
  const typed = secret.action === "set" ? secret.value : "";
  const clearing = secret.action === "clear";
  const chip = clearing ? "apiKeyWillClear" : configured ? "apiKeyStored" : "apiKeyNotStored";
  return <div class="field field-wide api-key-field">
    <span>{t("apiKey")}</span>
    <div class="api-key-row">
      <input autocomplete="off" disabled={disabled || clearing} placeholder={t("apiKeyPlaceholder")} spellcheck={false} type={revealed ? "text" : "password"} value={typed} onInput={(event) => onChange(event.currentTarget.value === "" ? { action: "keep" } : { action: "set", value: event.currentTarget.value })} />
      <button class="icon-button" disabled={typed === ""} type="button" onClick={() => setRevealed(!revealed)}>{revealed ? t("apiKeyHide") : t("apiKeyShow")}</button>
      <button class="icon-button" disabled={disabled} type="button" onClick={() => onChange(clearing ? { action: "keep" } : { action: "clear" })}>{clearing ? t("apiKeyUndoClear") : t("apiKeyClear")}</button>
    </div>
    <p class={`api-key-note ${chip}`}>{t(chip)}</p>
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

export function ProviderStatusRow({ disabled, error, passed, state, t, test }: { disabled: boolean; error: string | null; passed: boolean; state: ProviderTestState; t: Translate; test: () => void }) {
  const label = state === "failed" && error ? error : t(state === "testing" ? "providerTesting" : state !== "verified" ? "providerUnverified" : passed ? "providerTestPassed" : "providerVerified");
  return <div class="provider-status">
    <button class="icon-button" disabled={disabled || state === "testing"} type="button" onClick={test}>{t("providerTestConnection")}</button>
    <p class={`provider-status-note ${state}`} role={state === "failed" ? "alert" : undefined}><i />{label}</p>
  </div>;
}

export function MeetingNotesProviderSection({ blocked, onSecret, secret, settings, status, t, update }: { blocked: boolean; onSecret: (secret: SecretUpdate) => void; secret: SecretUpdate; settings: AppSettings; status: ComponentChildren; t: Translate; update: (mutate: (next: AppSettings) => void) => void }) {
  return <SettingsSection icon="chat" title={t("meetingNotesProvider")}>
    <p class="section-note">{t("meetingNotesProviderHint")}</p>
    <div class="form-grid">
      <TextField label={t("meetingNotesModel")} value={settings.meetingNotes.model} onChange={(value) => update((next) => { next.meetingNotes.model = value; })} />
      <TextField label={t("meetingNotesEndpoint")} type="url" value={settings.meetingNotes.endpoint} onChange={(value) => update((next) => { next.meetingNotes.endpoint = value; })} />
      <ApiKeyField configured={settings.meetingNotes.apiKeyConfigured} secret={secret} t={t} onChange={onSecret} />
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

export function GlobalContextSection({ busy, content, notice, onChange, revision, save, t }: { busy: boolean; content: GlobalContextContent; notice: string | null; onChange: (content: GlobalContextContent) => void; revision: number; save: () => void; t: Translate }) {
  const characters = contextCharacterCount(content);
  return <SettingsSection icon="book" title={t("globalContext")} action={<span class="context-count">{format(t("contextCharacters"), { characters, limit: CONTEXT_CHARACTER_LIMIT })}</span>}>
    <p class="section-note">{t("globalContextHint")}</p>
    {notice && <p class="inline-warning" role="alert">{notice}</p>}
    <ContextEditor kind="global" t={t} value={content} onChange={onChange} />
    <div class="context-actions"><span class="context-revision">r{revision}</span><button class="secondary-button accent" disabled={busy || characters > CONTEXT_CHARACTER_LIMIT} type="button" onClick={save}>{t("saveGlobalContext")}</button></div>
  </SettingsSection>;
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
  locale: string;
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
          <div class="context-actions"><span class="context-revision">r{revision}</span><button class="secondary-button accent" disabled={busy} type="button" onClick={() => saveContext(draft)}>{t("saveMeetingContext")}</button></div>
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
