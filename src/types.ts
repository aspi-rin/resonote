export type ThemeMode = "system" | "light" | "dark";
export type AppLanguage = "system" | "zh-CN" | "en-US";
export type AudioSourceMode = "microphone" | "system" | "mixed";
export type AudioFormat = "flac" | "wav";
export type RecordingPhase = "idle" | "starting" | "recording" | "stopping" | "failed";

export interface VadConfig {
  activationThreshold: number;
  maxSegmentMs: number;
  minSilenceMs: number;
  minSpeechMs: number;
}

/** `apiKey` carries the stored key, but only while it is still bound to
 *  `endpoint`; it is empty for every other provider state. */
export interface ProviderSettingsView {
  apiKey: string;
  apiKeyConfigured: boolean;
  endpoint: string;
  model: string;
  verified: boolean;
}

export type ProviderKind = "meetingNotes" | "translation";

export interface AppSettings {
  audio: {
    format: AudioFormat;
    microphoneGain: number;
    outputDirectory: string | null;
    segmentMinutes: number;
    source: AudioSourceMode;
    systemGain: number;
  };
  desktop: {
    hideWindowOnClose: boolean;
    language: AppLanguage;
    launchAtLogin: boolean;
    startRecordingOnLaunch: boolean;
    theme: ThemeMode;
  };
  meetingNotes: ProviderSettingsView & {
    maxInputCharacters: number;
    requestTimeoutSeconds: number;
  };
  transcription: {
    language: string;
    modelId: string;
    threads: number;
    unloadAfterIdleMinutes: number;
    vad: VadConfig;
  };
  translation: ProviderSettingsView & {
    enabled: boolean;
    targetLanguage: string;
  };
}

export type SecretUpdate = { action: "keep" } | { action: "set"; value: string } | { action: "clear" };

export interface SettingsSecretUpdates {
  meetingNotesApiKey: SecretUpdate;
  translationApiKey: SecretUpdate;
}

export interface AppSettingsWithoutSecrets extends Omit<AppSettings, "meetingNotes" | "translation"> {
  meetingNotes: Omit<AppSettings["meetingNotes"], "apiKey" | "apiKeyConfigured" | "verified">;
  translation: Omit<AppSettings["translation"], "apiKey" | "apiKeyConfigured" | "verified">;
}

/** Shared by `test_provider_settings` and `list_provider_models`. */
export interface TestProviderSettingsRequest {
  provider: ProviderKind;
  secrets: SettingsSecretUpdates;
  settings: AppSettingsWithoutSecrets;
}

export interface RecordingStatus {
  audioSource: AudioSourceMode;
  capturedSamples: number;
  elapsedMs: number;
  error: string | null;
  microphoneDb: number;
  microphoneDevice: string | null;
  microphoneWaveform: number[];
  phase: RecordingPhase;
  segmentCount: number;
  sessionDirectory: string | null;
  sessionId: string | null;
  systemDb: number;
  systemDevice: string | null;
  systemWaveform: number[];
  transcriptionError: string | null;
  vadProbability: number;
}

export interface ModelDownloadStatus {
  currentFile: string | null;
  downloadedBytes: number;
  error: string | null;
  modelId: string;
  phase: "missing" | "downloading" | "downloaded" | "failed" | "cancelled";
  totalBytes: number;
}

export interface ModelCatalogEntry {
  displayName: string;
  id: string;
  totalBytes: number;
}

export type TranscriptSegmentStatus = "pending" | "processing" | "complete" | "failed";
export type TranscriptDocumentStatus = "pending" | "processing" | "complete" | "partial";

export interface TranscriptSegment {
  attempts: number;
  audioFile: string;
  detectedLanguage: string;
  endMs: number;
  error: string | null;
  id: number;
  peakProbability: number;
  startMs: number;
  status: TranscriptSegmentStatus;
  text: string;
}

export interface TranscriptSegmentUpdate {
  segment: TranscriptSegment;
  sessionId: string;
}

export type TranslationSegmentStatus = "pending" | "processing" | "complete" | "failed";
export type TranslationDocumentStatus = "pending" | "processing" | "complete" | "partial";

export interface TranslationSegment {
  attempts: number;
  error: string | null;
  segmentId: number;
  sourceText: string;
  status: TranslationSegmentStatus;
  text: string;
}

export interface TranslationSegmentUpdate {
  segment: TranslationSegment;
  sessionId: string;
}

export interface TranslationDocument {
  endpoint: string;
  model: string;
  schemaVersion: number;
  segments: TranslationSegment[];
  sessionId: string;
  status: TranslationDocumentStatus;
  targetLanguage: string;
  updatedAt: string;
}

export interface TranscriptDocument {
  forcedLanguage: string;
  modelId: string;
  schemaVersion: number;
  segments: TranscriptSegment[];
  sessionId: string;
  status: TranscriptDocumentStatus;
  threads: number;
  unloadAfterIdleMinutes: number;
  updatedAt: string;
}

export interface TranscriptionStatus {
  currentSessionId: string | null;
  error: string | null;
  modelId: string | null;
  modelLoaded: boolean;
  pendingSegments: number;
  phase: "idle" | "waitingForModel" | "loadingModel" | "transcribing" | "failed";
}

export type ContextOrigin = "manual" | "documentDraft";

export interface ContextOriginMetadata {
  locator?: string;
  origin: ContextOrigin;
  sourceId?: string;
}

export interface ContextEntity {
  aliases: string[];
  canonicalName: string;
  commonAsrErrors: string[];
  description: string;
  id: string;
  origin?: ContextOriginMetadata;
}

export interface GlossaryEntry {
  aliases: string[];
  commonAsrErrors: string[];
  id: string;
  meaning: string;
  origin?: ContextOriginMetadata;
  term: string;
}

export interface ContextIdentity {
  aliases: string[];
  canonicalName: string;
  commonAsrErrors: string[];
}

export interface GlobalContextContent {
  freeText: string;
  glossary: GlossaryEntry[];
  identity: ContextIdentity;
  knowledgeBackground: string;
  preferredLanguages: string[];
  recurringOrganizations: ContextEntity[];
  recurringPeople: ContextEntity[];
  recurringProductsAndProjects: ContextEntity[];
  rolesAndAffiliations: string[];
  timezone: string;
}

export interface ParticipantContext extends ContextEntity {
  role: string;
  speakerLabel: string | null;
}

export interface MeetingContextContent {
  agenda: string[];
  background: string;
  constraints: string[];
  date: string;
  freeText: string;
  glossary: GlossaryEntry[];
  keyNumbers: string[];
  participants: ParticipantContext[];
  priorFactsAndDecisions: string[];
  purpose: string;
  questionsToDiscuss: string[];
  sourceNotes: string[];
  title: string;
}

export interface GlobalContextDocument {
  content: GlobalContextContent;
  revision: number;
  schemaVersion: number;
  updatedAt: string;
}

export interface VersionedMeetingContext {
  content: MeetingContextContent;
  revision: number;
  updatedAt: string;
}

export type AnalysisState =
  | "draft"
  | "queued"
  | "cleaning"
  | "summarizing"
  | "complete"
  | "cancelled"
  | "failed";
export type AnalysisFreshness = "none" | "fresh" | "stale";
export type StaleReason =
  | "globalContextChanged"
  | "meetingContextChanged"
  | "transcriptChanged"
  | "providerChanged"
  | "outputLanguageChanged"
  | "pipelineChanged";
export type OutputLanguage = "zh-CN" | "en-US";
export type RunStage = "cleaning" | "summarizing";

export interface SourceSegmentSnapshot {
  endMs: number;
  id: number;
  startMs: number;
  text: string;
}

export interface SourceSnapshot {
  completeSegmentIds: number[];
  completedSegmentCount: number;
  failedSegmentCount: number;
  failedSegmentIds: number[];
  selectedSegments: SourceSegmentSnapshot[];
  sessionStatus: "completed" | "interrupted" | "failed";
  skippedEmptySegmentIds: number[];
  skippedSegmentCount: number;
  transcriptSha256: string;
  transcriptStatus: "complete" | "partial";
}

export interface ContextSnapshot {
  capturedAt: string;
  global: GlobalContextContent;
  globalRevision: number;
  meeting: MeetingContextContent;
  meetingRevision: number;
  mergePolicyVersion: string;
  renderedSha256: string;
  schemaVersion: number;
}

export interface ProviderSnapshot {
  authMode: "none" | "bearer";
  endpoint: string;
  maxInputCharacters: number;
  model: string;
  requestTimeoutSeconds: number;
}

export interface InputSnapshot {
  chunkerVersion: string;
  context: ContextSnapshot;
  outputLanguage: OutputLanguage;
  pipelineVersion: string;
  promptVersion: string;
  provider: ProviderSnapshot;
  source: SourceSnapshot;
}

export interface RunError {
  causeCode: MeetingNotesErrorCode | null;
  code: MeetingNotesErrorCode;
  httpStatus: number | null;
  messageKey: string;
  retryable: boolean;
  stage: RunStage | null;
}

export interface RunProgress {
  completedChunks: number;
  totalChunks: number;
}

export interface CleanedPart {
  partIndex: number;
  segmentId: number;
  text: string;
}

export interface CleanPartResult {
  parts: CleanedPart[];
}

export interface CleanedSegment {
  endMs: number;
  segmentId: number;
  startMs: number;
  text: string;
}

export interface CleanResult {
  segments: CleanedSegment[];
}

export interface SourcedStatement {
  sourceSegmentIds: number[];
  text: string;
}

export interface BackgroundStatement {
  contextPaths: string[];
  text: string;
}

export interface ActionItem {
  dueDate: string | null;
  owner: string | null;
  sourceSegmentIds: number[];
  task: string;
}

export interface MeetingSummary {
  actionItems: ActionItem[];
  background: BackgroundStatement[];
  decisions: SourcedStatement[];
  keyPoints: SourcedStatement[];
  openQuestions: SourcedStatement[];
  overview: SourcedStatement;
  title: string;
}

export interface InputQuality {
  completedSegmentCount: number;
  failedSegmentCount: number;
  kind: "complete" | "partial";
  skippedSegmentCount: number;
}

export interface AnalysisResult {
  cleaned: CleanResult;
  generatedAt: string;
  inputFingerprint: string;
  inputQuality: InputQuality;
  inputSnapshot: InputSnapshot;
  outputLanguage: OutputLanguage;
  runFingerprint: string;
  summary: MeetingSummary;
}

export interface MeetingNotesRunView {
  cancellationRequested: boolean;
  createdAt: string;
  error: RunError | null;
  generation: number;
  inputFingerprint: string;
  jobId: string;
  progress: RunProgress;
  runFingerprint: string;
  stage: RunStage | null;
  state: AnalysisState;
  updatedAt: string;
}

export interface SessionAnalysisView {
  currentRun: MeetingNotesRunView | null;
  documentRevision: number;
  freshness: AnalysisFreshness;
  lastSuccessfulResult: AnalysisResult | null;
  meetingContext: VersionedMeetingContext;
  sessionId: string;
  staleReasons: StaleReason[];
}

export type MeetingNotesErrorCode =
  | "MEETING_NOTES_NOT_CONFIGURED"
  | "INVALID_ENDPOINT"
  | "INSECURE_ENDPOINT"
  | "RESPONSE_TOO_LARGE"
  | "SESSION_NOT_FOUND"
  | "SESSION_ID_CONFLICT"
  | "SESSION_DELETED"
  | "SESSION_STILL_RECORDING"
  | "TRANSCRIPT_NOT_READY"
  | "TRANSCRIPT_DOCUMENT_CORRUPT"
  | "TRANSCRIPT_INVALID"
  | "NO_TRANSCRIPT_CONTENT"
  | "PARTIAL_CONFIRMATION_REQUIRED"
  | "CONTEXT_DRAFT_INVALID"
  | "CONTEXT_REVISION_CONFLICT"
  | "CONTEXT_TOO_LARGE"
  | "PROVIDER_CHANGED"
  | "PROVIDER_UNAUTHORIZED"
  | "PROVIDER_FORBIDDEN"
  | "PROVIDER_RATE_LIMITED"
  | "PROVIDER_TIMEOUT"
  | "PROVIDER_UNAVAILABLE"
  | "PROVIDER_RESPONSE_INVALID"
  | "ANALYSIS_BUSY"
  | "CLEAN_OUTPUT_INVALID"
  | "SUMMARY_OUTPUT_INVALID"
  | "SUMMARY_REDUCE_DID_NOT_CONVERGE"
  | "SUMMARY_CANDIDATE_TOO_LARGE"
  | "RETRY_EXHAUSTED"
  | "ANALYSIS_CHECKPOINT_CORRUPT"
  | "ANALYSIS_DOCUMENT_CORRUPT"
  | "IO_ERROR";

export interface MeetingNotesErrorPayload {
  code: MeetingNotesErrorCode;
  messageKey: string;
  params: Record<string, number>;
  retryable: boolean;
}

/** Task metadata only: text, summaries and context come from the read commands. */
export interface MeetingNotesStatusEvent {
  completedChunks: number;
  documentRevision: number;
  errorCode: MeetingNotesErrorCode | null;
  generation: number;
  jobId: string;
  retryable: boolean;
  sessionId: string;
  stage: RunStage | null;
  state: AnalysisState;
  totalChunks: number;
  updatedAt: string;
}

export type GenerateMode = "ensure" | "regenerate";

export interface GenerateSessionAnalysisRequest {
  acceptPartial: boolean;
  expectedGlobalContextRevision: number;
  expectedMeetingContextRevision: number;
  mode: GenerateMode;
  outputLanguage: OutputLanguage;
  sessionId: string;
}

export interface RetrySessionAnalysisRequest {
  expectedRunFingerprint: string;
  jobId: string;
  outputLanguage: OutputLanguage;
  sessionId: string;
}

export type CancelSessionAnalysisRequest = RetrySessionAnalysisRequest;

export interface HistoryEntry {
  audioFormat: AudioFormat;
  audioSource: AudioSourceMode;
  completedAt: string | null;
  directory: string;
  durationMs: number;
  segmentCount: number;
  sessionId: string;
  startedAt: string;
  status: "recording" | "completed" | "interrupted" | "failed";
  transcriptPreview: string;
  transcriptSegmentCount: number;
  transcriptStatus: "pending" | "processing" | "complete" | "partial" | null;
  translationPreview: string;
  translationStatus: TranslationDocumentStatus | null;
  translationTargetLanguage: string | null;
}

export const DEFAULT_SETTINGS: AppSettings = {
  audio: {
    format: "flac",
    microphoneGain: 1,
    outputDirectory: null,
    segmentMinutes: 60,
    source: "mixed",
    systemGain: 1,
  },
  desktop: {
    hideWindowOnClose: true,
    language: "system",
    launchAtLogin: false,
    startRecordingOnLaunch: false,
    theme: "system",
  },
  meetingNotes: {
    apiKey: "",
    apiKeyConfigured: false,
    endpoint: "http://127.0.0.1:8000/v1",
    maxInputCharacters: 48_000,
    model: "",
    requestTimeoutSeconds: 180,
    verified: false,
  },
  transcription: {
    language: "auto",
    modelId: "qwen3-asr-0.6b-int8",
    threads: 6,
    unloadAfterIdleMinutes: 10,
    vad: {
      activationThreshold: 0.5,
      maxSegmentMs: 30_000,
      minSilenceMs: 500,
      minSpeechMs: 250,
    },
  },
  translation: {
    apiKey: "",
    apiKeyConfigured: false,
    enabled: true,
    endpoint: "http://127.0.0.1:8000/v1",
    model: "Hy-MT2-1.8B",
    targetLanguage: "Chinese",
    verified: false,
  },
};

export const EMPTY_GLOBAL_CONTEXT: GlobalContextContent = {
  freeText: "",
  glossary: [],
  identity: {
    aliases: [],
    canonicalName: "",
    commonAsrErrors: [],
  },
  knowledgeBackground: "",
  preferredLanguages: [],
  recurringOrganizations: [],
  recurringPeople: [],
  recurringProductsAndProjects: [],
  rolesAndAffiliations: [],
  timezone: "",
};

export const EMPTY_MEETING_CONTEXT: MeetingContextContent = {
  agenda: [],
  background: "",
  constraints: [],
  date: "",
  freeText: "",
  glossary: [],
  keyNumbers: [],
  participants: [],
  priorFactsAndDecisions: [],
  purpose: "",
  questionsToDiscuss: [],
  sourceNotes: [],
  title: "",
};

export const DEFAULT_RECORDING_STATUS: RecordingStatus = {
  audioSource: "mixed",
  capturedSamples: 0,
  elapsedMs: 0,
  error: null,
  microphoneDb: -80,
  microphoneDevice: null,
  microphoneWaveform: Array.from({ length: 48 }, () => 0),
  phase: "idle",
  segmentCount: 0,
  sessionDirectory: null,
  sessionId: null,
  systemDb: -80,
  systemDevice: null,
  systemWaveform: Array.from({ length: 48 }, () => 0),
  transcriptionError: null,
  vadProbability: 0,
};

export const DEFAULT_MODEL_STATUS: ModelDownloadStatus = {
  currentFile: null,
  downloadedBytes: 0,
  error: null,
  modelId: "qwen3-asr-0.6b-int8",
  phase: "missing",
  totalBytes: 0,
};

export const DEFAULT_TRANSCRIPTION_STATUS: TranscriptionStatus = {
  currentSessionId: null,
  error: null,
  modelId: null,
  modelLoaded: false,
  pendingSegments: 0,
  phase: "idle",
};
