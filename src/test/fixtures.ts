import {
  DEFAULT_MODEL_STATUS,
  DEFAULT_RECORDING_STATUS,
  DEFAULT_SETTINGS,
  DEFAULT_TRANSCRIPTION_STATUS,
  EMPTY_GLOBAL_CONTEXT,
  EMPTY_MEETING_CONTEXT,
  type AnalysisResult,
  type AppSettings,
  type AppSettingsWithoutSecrets,
  type CleanedSegment,
  type GlobalContextContent,
  type GlobalContextDocument,
  type HistoryEntry,
  type InputSnapshot,
  type MeetingContextContent,
  type MeetingNotesErrorPayload,
  type MeetingNotesRunView,
  type MeetingNotesStatusEvent,
  type MeetingSummary,
  type SecretUpdate,
  type SessionAnalysisView,
  type SettingsSecretUpdates,
  type TranscriptDocument,
  type TranscriptSegment,
} from "../types";
import { script } from "./tauri";

export const SESSION_ID = "session-1";
const STARTED_AT = "2026-08-15T02:00:00.000Z";

export function appSettings(mutate?: (next: AppSettings) => void): AppSettings {
  const settings = structuredClone(DEFAULT_SETTINGS);
  mutate?.(settings);
  return settings;
}

export function globalContent(overrides: Partial<GlobalContextContent> = {}): GlobalContextContent {
  return { ...structuredClone(EMPTY_GLOBAL_CONTEXT), ...overrides };
}

export function meetingContent(overrides: Partial<MeetingContextContent> = {}): MeetingContextContent {
  return { ...structuredClone(EMPTY_MEETING_CONTEXT), ...overrides };
}

export function globalContextDocument(overrides: Partial<GlobalContextDocument> = {}): GlobalContextDocument {
  return { content: globalContent(), revision: 0, schemaVersion: 1, updatedAt: STARTED_AT, ...overrides };
}

export function historyEntry(overrides: Partial<HistoryEntry> = {}): HistoryEntry {
  return {
    audioFormat: "flac",
    audioSource: "mixed",
    completedAt: "2026-08-15T02:30:00.000Z",
    directory: "/recordings/session-1",
    durationMs: 1_800_000,
    segmentCount: 1,
    sessionId: SESSION_ID,
    startedAt: STARTED_AT,
    status: "completed",
    transcriptPreview: "我们确认下季度优先级。",
    transcriptSegmentCount: 2,
    transcriptStatus: "complete",
    translationPreview: "",
    translationStatus: null,
    translationTargetLanguage: null,
    ...overrides,
  };
}

export function transcriptSegment(overrides: Partial<TranscriptSegment> = {}): TranscriptSegment {
  return {
    attempts: 1,
    audioFile: "audio-000.flac",
    detectedLanguage: "zh",
    endMs: 9_000,
    error: null,
    id: 1,
    peakProbability: 0.9,
    startMs: 5_000,
    status: "complete",
    text: "我们确认下季度优先级。",
    ...overrides,
  };
}

export function transcriptDocument(segments: TranscriptSegment[] = [transcriptSegment()], overrides: Partial<TranscriptDocument> = {}): TranscriptDocument {
  return {
    forcedLanguage: "auto",
    modelId: "qwen3-asr-0.6b-int8",
    schemaVersion: 1,
    segments,
    sessionId: SESSION_ID,
    status: "complete",
    threads: 6,
    unloadAfterIdleMinutes: 10,
    updatedAt: STARTED_AT,
    ...overrides,
  };
}

export const CLEANED_SEGMENTS: CleanedSegment[] = [
  { endMs: 9_000, segmentId: 1, startMs: 5_000, text: "我们确认下季度优先级。" },
  { endMs: 70_000, segmentId: 2, startMs: 65_000, text: "预算保持不变。" },
];

export function meetingSummary(overrides: Partial<MeetingSummary> = {}): MeetingSummary {
  return {
    actionItems: [{ dueDate: "2026-08-20", owner: "张三", sourceSegmentIds: [2], task: "更新路线图文档" }],
    background: [{ contextPaths: ["meeting.priorFactsAndDecisions[0]"], text: "上次会议已确定延期一周" }],
    decisions: [{ sourceSegmentIds: [2], text: "决定冻结本季度预算" }],
    keyPoints: [{ sourceSegmentIds: [1], text: "下季度优先级已对齐" }],
    openQuestions: [],
    overview: { sourceSegmentIds: [1], text: "会议确认了下季度优先级与预算安排。" },
    title: "Q3 路线图评审",
    ...overrides,
  };
}

export function inputSnapshot(overrides: Partial<InputSnapshot> = {}): InputSnapshot {
  return {
    chunkerVersion: "1",
    context: {
      capturedAt: STARTED_AT,
      global: globalContent({ knowledgeBackground: "快照全局背景" }),
      globalRevision: 1,
      meeting: meetingContent({ title: "快照会议标题" }),
      meetingRevision: 1,
      mergePolicyVersion: "1",
      renderedSha256: "sha-context",
      schemaVersion: 1,
    },
    outputLanguage: "zh-CN",
    pipelineVersion: "1",
    promptVersion: "1",
    provider: {
      authMode: "bearer",
      endpoint: "https://api.example.com/v1",
      maxInputCharacters: 48_000,
      model: "meeting-notes-model",
      requestTimeoutSeconds: 180,
    },
    source: {
      completeSegmentIds: [1, 2],
      completedSegmentCount: 2,
      failedSegmentCount: 0,
      failedSegmentIds: [],
      selectedSegments: [],
      sessionStatus: "completed",
      skippedEmptySegmentIds: [],
      skippedSegmentCount: 0,
      transcriptSha256: "sha-transcript",
      transcriptStatus: "complete",
    },
    ...overrides,
  };
}

export function analysisResult(overrides: Partial<AnalysisResult> = {}): AnalysisResult {
  return {
    cleaned: { segments: CLEANED_SEGMENTS },
    generatedAt: "2026-08-15T03:00:00.000Z",
    inputFingerprint: "fingerprint-input",
    inputQuality: { completedSegmentCount: 2, failedSegmentCount: 0, kind: "complete", skippedSegmentCount: 0 },
    inputSnapshot: inputSnapshot(),
    outputLanguage: "zh-CN",
    runFingerprint: "fingerprint-run",
    summary: meetingSummary(),
    ...overrides,
  };
}

export function runView(overrides: Partial<MeetingNotesRunView> = {}): MeetingNotesRunView {
  return {
    cancellationRequested: false,
    createdAt: "2026-08-15T03:00:00.000Z",
    error: null,
    generation: 1,
    inputFingerprint: "fingerprint-input",
    jobId: "job-1",
    progress: { completedChunks: 1, totalChunks: 2 },
    runFingerprint: "fingerprint-run",
    stage: null,
    state: "complete",
    updatedAt: "2026-08-15T03:00:05.000Z",
    ...overrides,
  };
}

export function analysisView(overrides: Partial<SessionAnalysisView> = {}): SessionAnalysisView {
  return {
    currentRun: null,
    documentRevision: 1,
    freshness: "none",
    lastSuccessfulResult: null,
    meetingContext: { content: meetingContent(), revision: 0, updatedAt: STARTED_AT },
    sessionId: SESSION_ID,
    staleReasons: [],
    ...overrides,
  };
}

export function completeAnalysisView(overrides: Partial<SessionAnalysisView> = {}): SessionAnalysisView {
  return analysisView({
    currentRun: runView(),
    documentRevision: 3,
    freshness: "fresh",
    lastSuccessfulResult: analysisResult(),
    meetingContext: { content: meetingContent({ title: "当前会议标题" }), revision: 2, updatedAt: STARTED_AT },
    ...overrides,
  });
}

export function statusEvent(overrides: Partial<MeetingNotesStatusEvent> = {}): MeetingNotesStatusEvent {
  return {
    completedChunks: 0,
    documentRevision: 1,
    errorCode: null,
    generation: 1,
    jobId: "job-1",
    retryable: false,
    sessionId: SESSION_ID,
    stage: null,
    state: "queued",
    totalChunks: 2,
    updatedAt: "2026-08-15T03:00:00.000Z",
    ...overrides,
  };
}

export function errorPayload(overrides: Partial<MeetingNotesErrorPayload> = {}): MeetingNotesErrorPayload {
  return { code: "PROVIDER_TIMEOUT", messageKey: "meetingNotesErrorProviderTimeout", params: {}, retryable: true, ...overrides };
}

interface AppScript {
  analysis?: SessionAnalysisView;
  globalContext?: GlobalContextDocument;
  history?: HistoryEntry[];
  settings?: AppSettings;
  transcript?: TranscriptDocument | null;
}

/** Scripts every command App issues on startup plus the meeting-notes commands,
 *  modelling the write-only secret contract of save_settings. */
export function scriptAppDefaults(options: AppScript = {}) {
  const settings = options.settings ?? appSettings();
  const globalContext = options.globalContext ?? globalContextDocument();
  const analysis = options.analysis ?? analysisView();
  script("get_settings", () => settings);
  script("get_recording_status", () => DEFAULT_RECORDING_STATUS);
  script("list_transcription_models", () => [{ displayName: "Qwen3-ASR 0.6B INT8", id: settings.transcription.modelId, totalBytes: 879_346_277 }]);
  script("get_model_status", () => ({ ...DEFAULT_MODEL_STATUS, phase: "downloaded" }));
  script("get_transcription_status", () => DEFAULT_TRANSCRIPTION_STATUS);
  script("list_recording_history", () => options.history ?? []);
  script("get_global_context", () => globalContext);
  script("app_info", () => ({ version: "0.2.1-test" }));
  script("get_session_transcript", () => options.transcript ?? null);
  script("get_session_translation", () => null);
  script("get_session_analysis", () => analysis);
  script("generate_session_analysis", () => analysis);
  script("retry_session_analysis", () => analysis);
  script("cancel_session_analysis", () => analysis);
  script("save_session_context", (args) => ({ content: args.content, revision: analysis.meetingContext.revision + 1, updatedAt: STARTED_AT }));
  script("save_global_context", (args) => ({ content: args.content, revision: globalContext.revision + 1, schemaVersion: 1, updatedAt: STARTED_AT }));
  script("save_settings", (args) => persistedSettings(settings, args.settings as AppSettingsWithoutSecrets, args.secrets as SettingsSecretUpdates));
  return { analysis, globalContext, settings };
}

function persistedSettings(current: AppSettings, payload: AppSettingsWithoutSecrets, secrets: SettingsSecretUpdates): AppSettings {
  const configured = (stored: boolean, update: SecretUpdate) => {
    if (update.action === "set") return update.value.trim().length > 0;
    if (update.action === "clear") return false;
    return stored;
  };
  return {
    ...payload,
    meetingNotes: { ...payload.meetingNotes, apiKeyConfigured: configured(current.meetingNotes.apiKeyConfigured, secrets.meetingNotesApiKey) },
    translation: { ...payload.translation, apiKeyConfigured: configured(current.translation.apiKeyConfigured, secrets.translationApiKey) },
  };
}
