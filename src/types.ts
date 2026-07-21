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
  transcription: {
    language: string;
    modelId: string;
    threads: number;
    unloadAfterIdleMinutes: number;
    vad: VadConfig;
  };
  translation: {
    enabled: boolean;
    endpoint: string;
    model: string;
    targetLanguage: string;
  };
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
    enabled: true,
    endpoint: "http://127.0.0.1:8000/v1",
    model: "Hy-MT2-1.8B",
    targetLanguage: "Chinese",
  },
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
