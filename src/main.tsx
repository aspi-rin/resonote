import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { open } from "@tauri-apps/plugin-dialog";
import { render } from "preact";
import { useEffect, useMemo, useRef, useState } from "preact/hooks";
import { resolveLanguage, translator, type TranslationKey } from "./i18n";
import packageMetadata from "../package.json";
import {
  Icon,
  NumberField,
  RangeField,
  SelectField,
  SettingsSection,
  TextField,
  Toggle,
  formatBytes,
  formatDuration,
  isTauri,
  type IconName,
} from "./ui";
import {
  GlobalContextSection,
  MeetingNotesChips,
  MeetingNotesDetail,
  MeetingNotesProviderSection,
  EndpointWarning,
  ProviderEndpointFields,
  ProviderKeyField,
  ProviderModelField,
  ProviderErrorNote,
  ProviderTestButton,
  analysisStatusFromEvent,
  analysisStatusFromView,
  apiKeySecret,
  describeError,
  hasSpokenContent,
  isNewerAnalysisStatus,
  parseMeetingNotesError,
  providerDirty,
  providerTestState,
  secretBlocksSave,
  type AnalysisStatus,
  type ProviderFetchProps,
  type ProviderKeyProps,
  type ProviderTestProps,
} from "./meeting_notes_ui";
import {
  DEFAULT_MODEL_STATUS,
  DEFAULT_RECORDING_STATUS,
  DEFAULT_SETTINGS,
  DEFAULT_TRANSCRIPTION_STATUS,
  EMPTY_GLOBAL_CONTEXT,
  type AppSettings,
  type AppSettingsWithoutSecrets,
  type AudioSourceMode,
  type GenerateMode,
  type GlobalContextContent,
  type GlobalContextDocument,
  type HistoryEntry,
  type MeetingContextContent,
  type MeetingNotesStatusEvent,
  type ModelCatalogEntry,
  type ModelDownloadStatus,
  type OutputLanguage,
  type ProviderKind,
  type RecordingStatus,
  type SecretUpdate,
  type SessionAnalysisView,
  type SettingsSecretUpdates,
  type TranscriptDocument,
  type TranscriptSegment,
  type TranscriptSegmentUpdate,
  type TranscriptionStatus,
  type TranslationDocument,
  type TranslationSegment,
  type TranslationSegmentUpdate,
  type VersionedMeetingContext,
} from "./types";
import "./styles.css";

type Tab = "record" | "history" | "settings";

interface LiveTranscript {
  segments: TranscriptSegment[];
  sessionId: string | null;
  translations: TranslationSegment[];
}

const EMPTY_GLOBAL_CONTEXT_DOCUMENT: GlobalContextDocument = { content: EMPTY_GLOBAL_CONTEXT, revision: 0, schemaVersion: 1, updatedAt: "" };
const FALLBACK_MODEL_CATALOG: ModelCatalogEntry[] = [
  { displayName: "Qwen3-ASR 0.6B INT8 · Multilingual", id: "qwen3-asr-0.6b-int8", totalBytes: 879_346_277 },
  { displayName: "Qwen3-ASR 1.7B INT8 · High accuracy", id: "qwen3-asr-1.7b-int8", totalBytes: 2_404_866_275 },
  { displayName: "FunASR-Nano INT8 · Chinese, English, Japanese", id: "funasr-nano-int8", totalBytes: 842_374_465 },
  { displayName: "Whisper Large-v3 INT8 · Multilingual", id: "whisper-large-v3-int8", totalBytes: 1_069_126_342 },
];

const KEEP_SECRETS: SettingsSecretUpdates = { meetingNotesApiKey: { action: "keep" }, translationApiKey: { action: "keep" } };
const NO_TEST_ERRORS: Record<ProviderKind, string | null> = { meetingNotes: null, translation: null };
const NO_FETCHED_MODELS: Record<ProviderKind, string[]> = { meetingNotes: [], translation: [] };
/** No draft means the field still shows the key the backend last returned. */
const NO_KEY_DRAFTS: Record<ProviderKind, string | null> = { meetingNotes: null, translation: null };
/** How long the form stays quiet before an edit is written to disk. */
export const AUTOSAVE_DELAY_MS = 800;

function secretUpdates(secrets: Record<ProviderKind, SecretUpdate>): SettingsSecretUpdates {
  return { meetingNotesApiKey: secrets.meetingNotes, translationApiKey: secrets.translation };
}

function saveSettingsPayload(settings: AppSettings, secrets: SettingsSecretUpdates = KEEP_SECRETS) {
  const payload: AppSettingsWithoutSecrets = {
    audio: settings.audio,
    desktop: settings.desktop,
    meetingNotes: {
      endpoint: settings.meetingNotes.endpoint,
      maxInputCharacters: settings.meetingNotes.maxInputCharacters,
      model: settings.meetingNotes.model,
      requestTimeoutSeconds: settings.meetingNotes.requestTimeoutSeconds,
    },
    transcription: settings.transcription,
    translation: {
      enabled: settings.translation.enabled,
      endpoint: settings.translation.endpoint,
      model: settings.translation.model,
      targetLanguage: settings.translation.targetLanguage,
    },
  };
  return { secrets, settings: payload };
}

export function App() {
  const [tab, setTab] = useState<Tab>("record");
  const [settings, setSettings] = useState<AppSettings>(DEFAULT_SETTINGS);
  const [recording, setRecording] = useState<RecordingStatus>(DEFAULT_RECORDING_STATUS);
  const [models, setModels] = useState<ModelCatalogEntry[]>(FALLBACK_MODEL_CATALOG);
  const [model, setModel] = useState<ModelDownloadStatus>(DEFAULT_MODEL_STATUS);
  const [transcription, setTranscription] = useState<TranscriptionStatus>(DEFAULT_TRANSCRIPTION_STATUS);
  const [liveTranscript, setLiveTranscript] = useState<LiveTranscript>({ segments: [], sessionId: null, translations: [] });
  const liveTranscriptSession = useRef<string | null>(null);
  const selectedModelId = useRef(DEFAULT_SETTINGS.transcription.modelId);
  const modelSelectionVersion = useRef(0);
  const [history, setHistory] = useState<HistoryEntry[]>([]);
  const [version, setVersion] = useState(packageMetadata.version);
  const [loading, setLoading] = useState(true);
  const [busy, setBusy] = useState(false);
  const [modelTransitioning, setModelTransitioning] = useState(false);
  const [saved, setSaved] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [keyDrafts, setKeyDrafts] = useState<Record<ProviderKind, string | null>>(NO_KEY_DRAFTS);
  const [keyFocused, setKeyFocused] = useState(false);
  const autosaveTimer = useRef<number | null>(null);
  const rejectedSave = useRef<string | null>(null);
  const [confirmed, setConfirmed] = useState<AppSettings>(DEFAULT_SETTINGS);
  const [testErrors, setTestErrors] = useState<Record<ProviderKind, string | null>>(NO_TEST_ERRORS);
  const [testing, setTesting] = useState<ProviderKind | null>(null);
  const testInFlight = useRef(false);
  const [fetchedModels, setFetchedModels] = useState<Record<ProviderKind, string[]>>(NO_FETCHED_MODELS);
  const [fetchErrors, setFetchErrors] = useState<Record<ProviderKind, string | null>>(NO_TEST_ERRORS);
  const [fetchingModels, setFetchingModels] = useState<ProviderKind | null>(null);
  const fetchInFlight = useRef(false);
  const [globalContext, setGlobalContext] = useState<GlobalContextDocument>(EMPTY_GLOBAL_CONTEXT_DOCUMENT);
  const [globalDraft, setGlobalDraft] = useState<GlobalContextContent>(EMPTY_GLOBAL_CONTEXT);
  const [globalNotice, setGlobalNotice] = useState<string | null>(null);
  const [analyses, setAnalyses] = useState<Record<string, SessionAnalysisView>>({});
  const [analysisStatuses, setAnalysisStatuses] = useState<Record<string, AnalysisStatus>>({});
  const [analysisErrors, setAnalysisErrors] = useState<Record<string, unknown>>({});
  const [expanded, setExpanded] = useState<string | null>(null);
  const [analysisBusy, setAnalysisBusy] = useState<string | null>(null);
  const analysisStatusRef = useRef<Record<string, AnalysisStatus>>({});
  const analysisPending = useRef(false);
  const expandedRef = useRef<string | null>(null);
  const t = useMemo(() => translator(settings.desktop.language), [settings.desktop.language]);
  const locale = resolveLanguage(settings.desktop.language);

  // One derivation feeds the key field, the save payload and every guard: the
  // field value is the in-progress edit when there is one, and the update sent
  // to the backend is its difference against the last confirmed key.
  const apiKeys: Record<ProviderKind, string> = {
    meetingNotes: keyDrafts.meetingNotes ?? settings.meetingNotes.apiKey,
    translation: keyDrafts.translation ?? settings.translation.apiKey,
  };
  const secrets: Record<ProviderKind, SecretUpdate> = {
    meetingNotes: apiKeySecret(apiKeys.meetingNotes, settings.meetingNotes.endpoint, confirmed.meetingNotes),
    translation: apiKeySecret(apiKeys.translation, settings.translation.endpoint, confirmed.translation),
  };
  const blocked: Record<ProviderKind, boolean> = {
    meetingNotes: secretBlocksSave(settings.meetingNotes.endpoint, apiKeys.meetingNotes),
    translation: secretBlocksSave(settings.translation.endpoint, apiKeys.translation),
  };
  const pendingSave = saveSettingsPayload(settings, secretUpdates(secrets));
  const pendingSignature = JSON.stringify(pendingSave);
  // A rejected payload is never retried on its own: only a further edit, which
  // changes the signature, may reach the backend again.
  const autosaveReady = isTauri
    && pendingSignature !== JSON.stringify(saveSettingsPayload(confirmed))
    && rejectedSave.current !== pendingSignature
    && !blocked.meetingNotes
    && !blocked.translation
    && !busy
    && testing === null
    && fetchingModels === null;

  const cancelAutosave = () => {
    if (autosaveTimer.current === null) return;
    window.clearTimeout(autosaveTimer.current);
    autosaveTimer.current = null;
  };

  const refreshHistory = async () => {
    if (!isTauri) return;
    setHistory(await invoke<HistoryEntry[]>("list_recording_history", { limit: 100 }));
  };

  // Out-of-order status events and read responses are dropped instead of
  // overwriting a newer snapshot of the same session.
  const applyAnalysisStatus = (sessionId: string, candidate: AnalysisStatus) => {
    const existing = analysisStatusRef.current[sessionId];
    if (existing && !isNewerAnalysisStatus(candidate, existing)) return false;
    analysisStatusRef.current = { ...analysisStatusRef.current, [sessionId]: candidate };
    setAnalysisStatuses(analysisStatusRef.current);
    return true;
  };

  const applyAnalysis = (view: SessionAnalysisView) => {
    if (!applyAnalysisStatus(view.sessionId, analysisStatusFromView(view))) return;
    setAnalyses((current) => ({ ...current, [view.sessionId]: view }));
  };

  const setAnalysisError = (sessionId: string, reason: unknown) => setAnalysisErrors((current) => ({ ...current, [sessionId]: reason }));

  const loadAnalysis = async (sessionId: string) => {
    if (!isTauri) return;
    try {
      applyAnalysis(await invoke<SessionAnalysisView>("get_session_analysis", { outputLanguage: locale, sessionId }));
    } catch (reason) {
      setAnalysisError(sessionId, reason);
    }
  };

  const loadGlobalContext = async () => {
    if (!isTauri) return;
    const document = await invoke<GlobalContextDocument>("get_global_context");
    setGlobalContext(document);
    setGlobalDraft(document.content);
  };

  const toggleSession = (sessionId: string) => {
    const next = expanded === sessionId ? null : sessionId;
    expandedRef.current = next;
    setExpanded(next);
    if (!next) return;
    setAnalysisError(next, null);
    void loadAnalysis(next);
    void loadGlobalContext().catch((reason) => setError(describeError(t, reason)));
  };

  const runAnalysisCommand = async (sessionId: string, command: () => Promise<SessionAnalysisView>) => {
    if (!isTauri || analysisPending.current) return;
    analysisPending.current = true;
    setAnalysisBusy(sessionId);
    setAnalysisError(sessionId, null);
    try {
      applyAnalysis(await command());
    } catch (reason) {
      setAnalysisError(sessionId, reason);
      if (parseMeetingNotesError(reason)?.code === "CONTEXT_REVISION_CONFLICT") await loadAnalysis(sessionId);
    } finally {
      analysisPending.current = false;
      setAnalysisBusy(null);
    }
  };

  const saveMeetingContext = async (sessionId: string, content: MeetingContextContent) => {
    if (!isTauri) return;
    setAnalysisError(sessionId, null);
    try {
      await invoke<VersionedMeetingContext>("save_session_context", {
        content,
        expectedMeetingContextRevision: analyses[sessionId]?.meetingContext.revision ?? 0,
        sessionId,
      });
      await loadAnalysis(sessionId);
    } catch (reason) {
      setAnalysisError(sessionId, reason);
      if (parseMeetingNotesError(reason)?.code === "CONTEXT_REVISION_CONFLICT") await loadAnalysis(sessionId);
    }
  };

  // The meeting context is confirmed first so the backend only ever generates
  // from revisions the user has seen.
  const generateAnalysis = (sessionId: string, content: MeetingContextContent, options: { acceptPartial: boolean; mode: GenerateMode }) =>
    void runAnalysisCommand(sessionId, async () => {
      const meeting = await invoke<VersionedMeetingContext>("save_session_context", {
        content,
        expectedMeetingContextRevision: analyses[sessionId]?.meetingContext.revision ?? 0,
        sessionId,
      });
      return invoke<SessionAnalysisView>("generate_session_analysis", {
        request: {
          acceptPartial: options.acceptPartial,
          expectedGlobalContextRevision: globalContext.revision,
          expectedMeetingContextRevision: meeting.revision,
          mode: options.mode,
          outputLanguage: locale,
          sessionId,
        },
      });
    });

  const controlRun = (sessionId: string, command: "cancel_session_analysis" | "retry_session_analysis") => {
    const run = analyses[sessionId]?.currentRun;
    if (!run) return;
    void runAnalysisCommand(sessionId, () => invoke<SessionAnalysisView>(command, {
      request: { expectedRunFingerprint: run.runFingerprint, jobId: run.jobId, outputLanguage: locale, sessionId },
    }));
  };

  const saveGlobalContext = async () => {
    if (!isTauri) return;
    setBusy(true);
    setGlobalNotice(null);
    try {
      const document = await invoke<GlobalContextDocument>("save_global_context", { content: globalDraft, expectedGlobalContextRevision: globalContext.revision });
      setGlobalContext(document);
      setGlobalDraft(document.content);
      if (expandedRef.current) void loadAnalysis(expandedRef.current);
    } catch (reason) {
      setGlobalNotice(describeError(t, reason));
      if (parseMeetingNotesError(reason)?.code === "CONTEXT_REVISION_CONFLICT") {
        await loadGlobalContext();
        setGlobalNotice(t("contextConflictReloaded"));
      }
    } finally {
      setBusy(false);
    }
  };

  /** Backend answers are the only source of the confirmed provider state the
   *  connection status compares the form against. */
  const applyPersisted = (persisted: AppSettings) => {
    rejectedSave.current = null;
    setSettings(persisted);
    setConfirmed(persisted);
  };

  const load = async () => {
    setLoading(true);
    setError(null);
    if (!isTauri) {
      setLoading(false);
      return;
    }
    try {
      const loadedSettings = await invoke<AppSettings>("get_settings");
      selectedModelId.current = loadedSettings.transcription.modelId;
      const [loadedRecording, loadedModels, loadedModel, loadedTranscription, loadedHistory, loadedGlobalContext, info] =
        await Promise.all([
          invoke<RecordingStatus>("get_recording_status"),
          invoke<ModelCatalogEntry[]>("list_transcription_models"),
          invoke<ModelDownloadStatus>("get_model_status", { modelId: loadedSettings.transcription.modelId }),
          invoke<TranscriptionStatus>("get_transcription_status"),
          invoke<HistoryEntry[]>("list_recording_history", { limit: 100 }),
          invoke<GlobalContextDocument>("get_global_context"),
          invoke<{ version: string }>("app_info"),
        ]);
      applyPersisted(loadedSettings);
      setModels(loadedModels);
      liveTranscriptSession.current = loadedRecording.sessionId;
      setRecording(loadedRecording);
      setModel(loadedModel);
      setTranscription(loadedTranscription);
      setHistory(loadedHistory);
      setGlobalContext(loadedGlobalContext);
      setGlobalDraft(loadedGlobalContext.content);
      setVersion(info.version);
    } catch (reason) {
      setError(String(reason));
    } finally {
      setLoading(false);
    }
  };

  useEffect(() => {
    void load();
  }, []);

  useEffect(() => {
    if (!isTauri) return;
    let disposed = false;
    const subscriptions: UnlistenFn[] = [];
    void Promise.all([
      listen<RecordingStatus>("recording-status", (event) => {
        if (event.payload.sessionId) liveTranscriptSession.current = event.payload.sessionId;
        setRecording(event.payload);
        if (event.payload.phase === "idle") void refreshHistory();
      }),
      listen<ModelDownloadStatus>("model-download-status", (event) => {
        if (event.payload.modelId === selectedModelId.current) setModel(event.payload);
      }),
      listen<TranscriptionStatus>("transcription-status", (event) => setTranscription(event.payload)),
      listen<TranscriptSegmentUpdate>("transcript-segment", (event) => {
        setLiveTranscript((current) => {
          if (liveTranscriptSession.current !== event.payload.sessionId) return current;
          const segments = current.sessionId === event.payload.sessionId ? current.segments : [];
          return {
            segments: upsertSegment(segments, event.payload.segment),
            sessionId: event.payload.sessionId,
            translations: current.sessionId === event.payload.sessionId ? current.translations : [],
          };
        });
      }),
      listen<TranslationSegmentUpdate>("translation-segment", (event) => {
        setLiveTranscript((current) => {
          if (liveTranscriptSession.current !== event.payload.sessionId) return current;
          const translations = current.sessionId === event.payload.sessionId ? current.translations : [];
          return {
            segments: current.sessionId === event.payload.sessionId ? current.segments : [],
            sessionId: event.payload.sessionId,
            translations: upsertTranslation(translations, event.payload.segment),
          };
        });
        if (event.payload.segment.status === "complete" || event.payload.segment.status === "failed") {
          void refreshHistory();
        }
      }),
    ]).then((unlisteners) => {
      if (disposed) unlisteners.forEach((unlisten) => unlisten());
      else subscriptions.push(...unlisteners);
    });
    return () => {
      disposed = true;
      subscriptions.forEach((unlisten) => unlisten());
    };
  }, []);

  useEffect(() => {
    if (!isTauri) return;
    let disposed = false;
    let unlisten: UnlistenFn | null = null;
    void listen<MeetingNotesStatusEvent>("meeting-notes-status", (event) => {
      if (!applyAnalysisStatus(event.payload.sessionId, analysisStatusFromEvent(event.payload))) return;
      if (expandedRef.current === event.payload.sessionId) void loadAnalysis(event.payload.sessionId);
    }).then((subscription) => {
      if (disposed) subscription();
      else unlisten = subscription;
    });
    return () => {
      disposed = true;
      unlisten?.();
    };
  }, [locale]);

  useEffect(() => {
    const sessionId = recording.sessionId;
    if (!sessionId) return;
    liveTranscriptSession.current = sessionId;
    setLiveTranscript((current) => (current.sessionId === sessionId ? current : { segments: [], sessionId, translations: [] }));
    if (!isTauri) return;
    // Seed from disk so a window reload or crash recovery does not lose earlier sentences.
    void Promise.all([
      invoke<TranscriptDocument | null>("get_session_transcript", { sessionId }),
      invoke<TranslationDocument | null>("get_session_translation", { sessionId }),
    ])
      .then(([document, translation]) => {
        if (!document && !translation) return;
        setLiveTranscript((current) => {
          if (current.sessionId !== sessionId) return current;
          return {
            segments: document ? mergeSegments(document.segments, current.segments) : current.segments,
            sessionId,
            translations: translation ? mergeTranslations(translation.segments, current.translations) : current.translations,
          };
        });
      })
      .catch((reason) => console.warn("failed to seed live transcript and translation", reason));
  }, [recording.sessionId]);

  useEffect(() => {
    const root = document.documentElement;
    if (settings.desktop.theme === "system") root.removeAttribute("data-theme");
    else root.dataset.theme = settings.desktop.theme;
    root.lang = locale;
  }, [settings.desktop.theme, locale]);

  // Autosave: there is no save button, so every edit reaches disk once the form
  // has been quiet for a moment. A focused key field, a blocked provider and an
  // in-flight request all hold the timer back rather than cancel the edit.
  useEffect(() => {
    if (!autosaveReady || keyFocused) return;
    const timer = window.setTimeout(() => {
      autosaveTimer.current = null;
      void saveSettings();
    }, AUTOSAVE_DELAY_MS);
    autosaveTimer.current = timer;
    return () => {
      window.clearTimeout(timer);
      if (autosaveTimer.current === timer) autosaveTimer.current = null;
    };
  }, [autosaveReady, keyFocused, pendingSignature]);

  const update = (mutate: (next: AppSettings) => void) => {
    setSaved(false);
    setSettings((current) => {
      const next = structuredClone(current);
      mutate(next);
      return next;
    });
  };

  const updateApiKey = (provider: ProviderKind, value: string) => {
    setSaved(false);
    setKeyDrafts((current) => ({ ...current, [provider]: value }));
  };

  const saveSettings = async () => {
    cancelAutosave();
    const attempted = pendingSignature;
    setBusy(true);
    setError(null);
    try {
      const persisted = isTauri ? await invoke<AppSettings>("save_settings", pendingSave) : settings;
      applyPersisted(persisted);
      setKeyDrafts(NO_KEY_DRAFTS);
      setSaved(true);
      window.setTimeout(() => setSaved(false), 1800);
    } catch (reason) {
      rejectedSave.current = attempted;
      setError(String(reason));
    } finally {
      setBusy(false);
    }
  };

  // The blur of the key field is the moment its edit is finished, so it saves
  // now instead of waiting out another debounce.
  const focusApiKey = (focused: boolean) => {
    setKeyFocused(focused);
    if (focused) cancelAutosave();
    else if (autosaveReady) void saveSettings();
  };

  const testProvider = async (provider: ProviderKind) => {
    if (testInFlight.current || !isTauri) return;
    cancelAutosave();
    testInFlight.current = true;
    setTesting(provider);
    setError(null);
    setTestErrors((current) => ({ ...current, [provider]: null }));
    try {
      const persisted = await invoke<AppSettings>("test_provider_settings", { request: { provider, ...pendingSave } });
      applyPersisted(persisted);
      setKeyDrafts(NO_KEY_DRAFTS);
      setSaved(true);
      window.setTimeout(() => setSaved(false), 1800);
    } catch (reason) {
      setTestErrors((current) => ({ ...current, [provider]: describeError(t, reason) }));
    } finally {
      testInFlight.current = false;
      setTesting(null);
    }
  };

  // Discovery only: nothing is persisted, so the form keeps whatever the user
  // has typed whether the endpoint answers or not.
  const fetchProviderModels = async (provider: ProviderKind) => {
    if (fetchInFlight.current || testInFlight.current || !isTauri) return;
    fetchInFlight.current = true;
    setFetchingModels(provider);
    setFetchErrors((current) => ({ ...current, [provider]: null }));
    try {
      const models = await invoke<string[]>("list_provider_models", { request: { provider, ...pendingSave } });
      setFetchedModels((current) => ({ ...current, [provider]: models }));
    } catch (reason) {
      setFetchErrors((current) => ({ ...current, [provider]: describeError(t, reason) }));
    } finally {
      fetchInFlight.current = false;
      setFetchingModels(null);
    }
  };

  const startRecording = async () => {
    setBusy(true);
    setError(null);
    try {
      if (!isTauri) {
        setRecording({ ...DEFAULT_RECORDING_STATUS, phase: "recording" });
        return;
      }
      const persisted = await invoke<AppSettings>("save_settings", saveSettingsPayload(settings));
      applyPersisted(persisted);
      const started = await invoke<RecordingStatus>("start_recording");
      liveTranscriptSession.current = started.sessionId;
      setRecording(started);
    } catch (reason) {
      setError(String(reason));
    } finally {
      setBusy(false);
    }
  };

  const stopRecording = async () => {
    setBusy(true);
    setError(null);
    setRecording((current) => ({ ...current, phase: "stopping" }));
    try {
      if (!isTauri) setRecording(DEFAULT_RECORDING_STATUS);
      else {
        setRecording(await invoke<RecordingStatus>("stop_recording"));
        await refreshHistory();
      }
    } catch (reason) {
      setError(String(reason));
    } finally {
      setBusy(false);
    }
  };

  const changeAudioSource = async (source: AudioSourceMode) => {
    const isRecording = recording.phase === "recording";
    const currentSource = isRecording ? recording.audioSource : settings.audio.source;
    if (source === currentSource) return;
    const nextSettings = {
      ...settings,
      audio: { ...settings.audio, source },
    };
    setBusy(true);
    setError(null);
    try {
      if (isTauri && isRecording) {
        setRecording(await invoke<RecordingStatus>("set_recording_source", { source }));
      } else if (!isTauri && isRecording) {
        setRecording((current) => ({ ...current, audioSource: source }));
      }
      setSettings(nextSettings);
      if (isTauri) {
        applyPersisted(await invoke<AppSettings>("save_settings", saveSettingsPayload(nextSettings)));
      }
    } catch (reason) {
      setError(String(reason));
    } finally {
      setBusy(false);
    }
  };

  const changeRecordingLanguages = async (
    recognitionLanguage: string,
    translationEnabled: boolean,
    translationTargetLanguage: string,
  ) => {
    const nextSettings = structuredClone(settings);
    nextSettings.transcription.language = recognitionLanguage;
    nextSettings.translation.enabled = translationEnabled;
    nextSettings.translation.targetLanguage = translationTargetLanguage;
    if (recording.phase !== "recording") {
      setSaved(false);
      setSettings(nextSettings);
      return;
    }

    setBusy(true);
    setError(null);
    try {
      if (isTauri) {
        await invoke("set_recording_languages", {
          recognitionLanguage,
          translationEnabled,
          translationTargetLanguage,
        });
      }
      setSettings(nextSettings);
      if (isTauri) {
        applyPersisted(await invoke<AppSettings>("save_settings", saveSettingsPayload(nextSettings)));
      }
    } catch (reason) {
      setError(String(reason));
    } finally {
      setBusy(false);
    }
  };

  const installModel = () => {
    if (!isTauri) {
      setModel({ ...model, phase: "downloaded", downloadedBytes: 1, totalBytes: 1 });
      return;
    }
    setError(null);
    const modelId = settings.transcription.modelId;
    void invoke<ModelDownloadStatus>("install_model", { modelId })
      .then((status) => {
        if (status.modelId === selectedModelId.current) setModel(status);
      })
      .catch((reason) => {
        if (modelId === selectedModelId.current) setError(String(reason));
      });
  };

  const selectModel = async (modelId: string) => {
    const selectionVersion = ++modelSelectionVersion.current;
    const cancelCurrentDownload = model.phase === "downloading" && model.modelId !== modelId;
    const selected = models.find((item) => item.id === modelId);
    selectedModelId.current = modelId;
    update((next) => { next.transcription.modelId = modelId; });
    setModel({
      ...DEFAULT_MODEL_STATUS,
      modelId,
      totalBytes: selected?.totalBytes ?? 0,
    });
    if (!isTauri) {
      return;
    }
    setError(null);
    setModelTransitioning(true);
    try {
      if (cancelCurrentDownload) await invoke("cancel_model_install");
      const status = await invoke<ModelDownloadStatus>("get_model_status", { modelId });
      if (status.modelId === selectedModelId.current) setModel(status);
    } catch (reason) {
      if (modelId === selectedModelId.current) setError(String(reason));
    } finally {
      if (selectionVersion === modelSelectionVersion.current) setModelTransitioning(false);
    }
  };

  const cancelModel = async () => {
    setError(null);
    setModelTransitioning(true);
    try {
      await invoke("cancel_model_install");
    } catch (reason) {
      setError(String(reason));
    } finally {
      setModelTransitioning(false);
    }
  };

  const chooseOutputDirectory = async () => {
    if (!isTauri) return;
    setError(null);
    try {
      const selected = await open({
        defaultPath: settings.audio.outputDirectory ?? undefined,
        directory: true,
        multiple: false,
        title: t("output"),
      });
      if (typeof selected === "string") {
        update((next) => { next.audio.outputDirectory = selected; });
      }
    } catch (reason) {
      setError(String(reason));
    }
  };

  const deleteRecording = async (sessionId: string) => {
    if (!window.confirm(t("deleteConfirm"))) return;
    setError(null);
    try {
      if (isTauri) await invoke("delete_recording", { sessionId });
      setHistory((current) => current.filter((entry) => entry.sessionId !== sessionId));
    } catch (reason) {
      setError(String(reason));
    }
  };

  const isActive = recording.phase === "recording" || recording.phase === "starting" || recording.phase === "stopping";
  const phaseLabel = t((recording.phase === "idle" ? "ready" : recording.phase) as TranslationKey);

  if (loading) {
    return <main class="splash"><div class="brand-mark"><Icon name="wave" /></div><p>{t("loading")}</p></main>;
  }

  return (
    <div class="app-layout">
      <aside class="sidebar">
        <div class="brand"><div class="brand-mark"><Icon name="wave" /></div><div><strong>Resonote</strong><span>{t("appTagline")}</span></div></div>
        <nav class="nav-list" aria-label="Primary">
          <NavButton active={tab === "record"} icon="mic" label={t("record")} onClick={() => setTab("record")} />
          <NavButton active={tab === "history"} icon="history" label={t("history")} onClick={() => { setTab("history"); void refreshHistory(); }} />
          <NavButton active={tab === "settings"} icon="gear" label={t("settings")} onClick={() => setTab("settings")} />
        </nav>
        <span class="version">{t("version")} {version}</span>
      </aside>

      <main class="main-panel">
        <div class="main-content">
          <header class="topbar">
            <div><p class="eyebrow">RESONOTE / {t(tab)}</p><h1>{t(tab)}</h1></div>
            <span class={`phase-badge phase-${recording.phase}`}><i />{phaseLabel}</span>
          </header>

          {error && <div class="error-banner" role="alert"><strong>{t("error")}</strong><span>{error}</span><button onClick={() => setError(null)}>×</button></div>}

          {tab === "record" && <RecordView t={t} settings={settings} recording={recording} transcription={transcription} model={model} liveSegments={liveTranscript.segments} liveTranslations={liveTranscript.translations} busy={busy} isActive={isActive} changeLanguages={changeRecordingLanguages} changeSource={changeAudioSource} start={startRecording} stop={stopRecording} />}
          {tab === "history" && <HistoryView t={t} locale={locale} history={history} analyses={analyses} analysisBusy={analysisBusy} analysisErrors={analysisErrors} analysisStatuses={analysisStatuses} expanded={expanded} globalContext={globalContext.content} settings={settings} cancel={(sessionId) => controlRun(sessionId, "cancel_session_analysis")} generate={generateAnalysis} openSettings={() => setTab("settings")} retry={(sessionId) => controlRun(sessionId, "retry_session_analysis")} saveContext={(sessionId, content) => void saveMeetingContext(sessionId, content)} toggle={toggleSession} refresh={() => void refreshHistory()} open={(sessionId) => void invoke("open_recording_directory", { sessionId }).catch((reason) => setError(String(reason)))} remove={(sessionId) => void deleteRecording(sessionId)} />}
          {tab === "settings" && <SettingsView t={t} locale={locale} settings={settings} models={models} busy={busy || isActive} modelTransitioning={modelTransitioning} saved={saved} update={update} model={model} selectModel={(modelId) => void selectModel(modelId)} installModel={installModel} cancelModel={() => void cancelModel()} chooseOutputDirectory={() => void chooseOutputDirectory()} globalContext={globalDraft} globalNotice={globalNotice} globalRevision={globalContext.revision} saveGlobalContext={() => void saveGlobalContext()} providerFetch={{ errors: fetchErrors, fetch: (provider) => void fetchProviderModels(provider), models: fetchedModels, running: fetchingModels }} providerKeys={{ blocked, focus: focusApiKey, secrets, update: updateApiKey, values: apiKeys }} providerTest={{ confirmed, errors: testErrors, running: testing, test: (provider) => void testProvider(provider) }} updateGlobalContext={setGlobalDraft} />}
        </div>
      </main>
    </div>
  );
}

function NavButton({ active, icon, label, onClick }: { active: boolean; icon: IconName; label: string; onClick: () => void }) {
  return <button class={`nav-button ${active ? "active" : ""}`} onClick={onClick}><Icon name={icon} /><span>{label}</span></button>;
}

interface ViewProps { t: ReturnType<typeof translator> }

function RecordView({ t, settings, recording, transcription, model, liveSegments, liveTranslations, busy, isActive, changeLanguages, changeSource, start, stop }: ViewProps & {
  settings: AppSettings; recording: RecordingStatus; transcription: TranscriptionStatus; model: ModelDownloadStatus; liveSegments: TranscriptSegment[]; liveTranslations: TranslationSegment[]; busy: boolean; isActive: boolean;
  changeLanguages: (recognitionLanguage: string, translationEnabled: boolean, translationTargetLanguage: string) => void; changeSource: (source: AudioSourceMode) => void; start: () => void; stop: () => void;
}) {
  const currentSource = recording.phase === "recording" ? recording.audioSource : settings.audio.source;
  const sourceSwitchingDisabled = busy || recording.phase === "starting" || recording.phase === "stopping";
  const languageSwitchingDisabled = busy || recording.phase === "starting" || recording.phase === "stopping";
  return <div class="view-stack">
    <section class={`record-card ${isActive ? "active" : ""}`}>
      <div class="record-summary"><span class="record-label">{isActive ? t("recording") : t("ready")}</span><strong class="timer">{formatDuration(recording.elapsedMs)}</strong><span class="session-meta">{recording.segmentCount} {t("segments").toLowerCase()}</span></div>
      <label class="record-source transcript-control">
        <span>{t("source")}</span>
        <select disabled={sourceSwitchingDisabled} value={currentSource} onChange={(event) => changeSource(event.currentTarget.value as AudioSourceMode)}><option value="mixed">{t("mixed")}</option><option value="microphone">{t("microphone")}</option><option value="system">{t("system")}</option></select>
      </label>
      <div class="record-signal" role="group" aria-label={t("liveSignal")}>
        <div class="record-signal-heading"><strong>{t("liveSignal")}</strong><span>{t("voiceActivity")}: <b>{Math.round(recording.vadProbability * 100)}%</b></span></div>
        <Level label={t("microphone")} value={recording.microphoneDb} muted={currentSource === "system"} />
        <Level label={t("system")} value={recording.systemDb} muted={currentSource === "microphone"} />
      </div>
      <button class={`record-button ${isActive ? "stop" : ""}`} disabled={busy} onClick={isActive ? stop : start}><span class="record-button-icon" />{isActive ? t("stop") : t("start")}</button>
    </section>

    <LiveTranscriptPanel t={t} segments={liveSegments} translations={liveTranslations} settings={settings} transcription={transcription} model={model} disabled={languageSwitchingDisabled} changeLanguages={changeLanguages} />

  </div>;
}

const TRANSCRIPT_MIN_HEIGHT = 120;
const TRANSCRIPT_MAX_HEIGHT = 520;
const TRANSCRIPT_HEIGHT_STEP = 24;

function LiveTranscriptPanel({ t, segments, translations, settings, transcription, model, disabled, changeLanguages }: ViewProps & {
  segments: TranscriptSegment[];
  translations: TranslationSegment[];
  settings: AppSettings;
  transcription: TranscriptionStatus;
  model: ModelDownloadStatus;
  disabled: boolean;
  changeLanguages: (recognitionLanguage: string, translationEnabled: boolean, translationTargetLanguage: string) => void;
}) {
  const [height, setHeight] = useState(220);
  const scrollRef = useRef<HTMLDivElement>(null);
  const pinnedToBottom = useRef(true);

  useEffect(() => {
    const scroller = scrollRef.current;
    if (scroller && pinnedToBottom.current) scroller.scrollTop = scroller.scrollHeight;
  }, [segments, translations]);

  const trackScroll = () => {
    const scroller = scrollRef.current;
    if (scroller) pinnedToBottom.current = scroller.scrollHeight - scroller.scrollTop - scroller.clientHeight < 24;
  };

  const clampHeight = (value: number) => Math.min(TRANSCRIPT_MAX_HEIGHT, Math.max(TRANSCRIPT_MIN_HEIGHT, value));

  const beginResize = (event: preact.JSX.TargetedPointerEvent<HTMLDivElement>) => {
    event.preventDefault();
    const handle = event.currentTarget;
    const startY = event.clientY;
    const startHeight = height;
    handle.focus();
    handle.setPointerCapture(event.pointerId);
    const move = (pointer: PointerEvent) => setHeight(clampHeight(startHeight + pointer.clientY - startY));
    const finish = () => {
      handle.removeEventListener("pointermove", move);
      handle.removeEventListener("pointerup", finish);
      handle.removeEventListener("pointercancel", finish);
    };
    handle.addEventListener("pointermove", move);
    handle.addEventListener("pointerup", finish);
    handle.addEventListener("pointercancel", finish);
  };

  const nudgeResize = (event: preact.JSX.TargetedKeyboardEvent<HTMLDivElement>) => {
    if (!["ArrowUp", "ArrowDown", "Home", "End"].includes(event.key)) return;
    event.preventDefault();
    if (event.key === "Home") setHeight(TRANSCRIPT_MIN_HEIGHT);
    else if (event.key === "End") setHeight(TRANSCRIPT_MAX_HEIGHT);
    else {
      const delta = event.key === "ArrowUp" ? -TRANSCRIPT_HEIGHT_STEP : TRANSCRIPT_HEIGHT_STEP;
      setHeight((current) => clampHeight(current + delta));
    }
  };

  const updateTranslationLanguage = (value: string) => {
    changeLanguages(
      settings.transcription.language,
      value !== "off",
      value === "off" ? settings.translation.targetLanguage : value,
    );
  };

  return <section class="panel-card transcript-panel">
    <div class="section-heading"><div><span class="section-icon"><Icon name="text" /></span><div><h2>{t("liveTranscript")}</h2><p>{t("liveTranscriptHint")}</p></div></div><div class="transcript-heading-meta"><TranscriptionBadge t={t} status={transcription} model={model} /><div class="transcript-controls"><label class="transcript-control"><span>{t("recognitionLanguage")}</span><select disabled={disabled} value={settings.transcription.language} onChange={(event) => changeLanguages(event.currentTarget.value, settings.translation.enabled, settings.translation.targetLanguage)}><option value="auto">Auto</option><option value="zh-CN">{t("languageChineseShort")}</option><option value="Japanese">{t("languageJapaneseShort")}</option><option value="en-US">{t("languageEnglishShort")}</option></select></label><label class="transcript-control"><span>{t("translationLanguage")}</span><select disabled={disabled} value={settings.translation.enabled ? settings.translation.targetLanguage : "off"} onChange={(event) => updateTranslationLanguage(event.currentTarget.value)}><option value="off">{t("noTranslation")}</option><option value="Chinese">{t("languageChineseShort")}</option><option value="Japanese">{t("languageJapaneseShort")}</option><option value="English">{t("languageEnglishShort")}</option></select></label></div></div></div>
    <div class="transcript-scroll" style={{ height: `${height}px` }} ref={scrollRef} onScroll={trackScroll} aria-live="polite" aria-relevant="additions text">
      {segments.length === 0
        ? <p class="transcript-empty">{t("liveTranscriptEmpty")}</p>
        : <ol class="transcript-list">{segments.filter((segment) => segment.status !== "complete" || hasSpokenContent(segment.text)).map((segment) => { const translation = translations.find((item) => item.segmentId === segment.id); return <li class={`transcript-row ${segment.status}`} key={segment.id}><time>{formatDuration(segment.startMs)}</time><div class="transcript-copy"><span class="transcript-source">{segmentText(t, segment)}</span>{translation && <span class={`transcript-translation ${translation.status}`}>{translationText(t, translation)}</span>}</div></li>; })}</ol>}
    </div>
    <div class="transcript-resize" role="separator" aria-orientation="horizontal" aria-label={t("resizeTranscript")} aria-valuemin={TRANSCRIPT_MIN_HEIGHT} aria-valuemax={TRANSCRIPT_MAX_HEIGHT} aria-valuenow={height} tabIndex={0} onPointerDown={beginResize} onKeyDown={nudgeResize} />
  </section>;
}

function TranscriptionBadge({ t, status, model }: ViewProps & { status: TranscriptionStatus; model: ModelDownloadStatus }) {
  if (model.phase !== "downloaded") {
    const label = model.phase === "downloading" ? t("modelDownloading") : t("modelMissing");
    return <span class={`transcription-badge model-${model.phase}`} title={model.error ?? label} aria-live="polite"><i />{label}</span>;
  }
  if (status.phase !== "failed") return null;
  const label = t("transcriptionFailed");
  return <span class="transcription-badge failed" title={status.error ?? label} aria-live="polite"><i />{label}</span>;
}

export function HistoryView({ t, locale, history, analyses, analysisBusy, analysisErrors, analysisStatuses, expanded, globalContext, settings, cancel, generate, openSettings, retry, saveContext, toggle, refresh, open, remove }: ViewProps & {
  locale: OutputLanguage; history: HistoryEntry[]; analyses: Record<string, SessionAnalysisView>; analysisBusy: string | null; analysisErrors: Record<string, unknown>; analysisStatuses: Record<string, AnalysisStatus>; expanded: string | null; globalContext: GlobalContextContent; settings: AppSettings;
  cancel: (id: string) => void; generate: (id: string, content: MeetingContextContent, options: { acceptPartial: boolean; mode: GenerateMode }) => void; openSettings: () => void; retry: (id: string) => void; saveContext: (id: string, content: MeetingContextContent) => void; toggle: (id: string) => void; refresh: () => void; open: (id: string) => void; remove: (id: string) => void;
}) {
  return <div class="view-stack"><div class="view-actions"><p>{history.length} {t("history").toLowerCase()}</p><button class="icon-button" onClick={refresh}><Icon name="refresh" />{t("refresh")}</button></div>
    {history.length === 0 ? <section class="empty-state"><span><Icon name="history" /></span><h2>{t("noHistory")}</h2><p>{t("noHistoryHint")}</p></section> : <div class="history-list">{history.map((entry) => { const isOpen = expanded === entry.sessionId; return <div class="history-item" key={entry.sessionId}>
      <article class={`history-card ${isOpen ? "expanded" : ""}`} onClick={() => toggle(entry.sessionId)}><div class="history-date"><strong>{new Date(entry.startedAt).toLocaleDateString(locale, { month: "short", day: "numeric" })}</strong><span>{new Date(entry.startedAt).toLocaleTimeString(locale, { hour: "2-digit", minute: "2-digit" })}</span></div><div class="history-body"><div class="history-title"><strong>{sourceLabel(t, entry.audioSource)}</strong><span>{formatDuration(entry.durationMs)} · {entry.audioFormat.toUpperCase()}</span></div><p class="history-transcript">{entry.transcriptPreview || t("noTranscript")}</p>{entry.translationStatus && <p class="history-translation"><strong>{entry.translationTargetLanguage ? `${t("translateTo")} ${translationLanguageLabel(t, entry.translationTargetLanguage)}` : t("translation")}</strong><span>{entry.translationPreview || (entry.translationStatus === "partial" ? t("translationFailed") : t("translating"))}</span></p>}<div class="history-tags"><span>{t(entry.status === "recording" ? "recording" : entry.status === "interrupted" ? "interrupted" : entry.status === "failed" ? "failed" : "completed")}</span>{entry.transcriptStatus && <span>{t(entry.transcriptStatus === "complete" ? "transcriptComplete" : entry.transcriptStatus === "partial" ? "transcriptPartial" : "transcriptPending")}</span>}{entry.translationStatus && <span>{t(entry.translationStatus === "complete" ? "translationComplete" : entry.translationStatus === "partial" ? "translationPartial" : "translationPending")}</span>}<MeetingNotesChips analysis={analyses[entry.sessionId] ?? null} state={analysisStatuses[entry.sessionId]?.state ?? null} t={t} /></div></div><div class="history-actions"><button class="folder-button delete-button" disabled={entry.status === "recording"} title={t("deleteRecording")} onClick={(event) => { event.stopPropagation(); remove(entry.sessionId); }}><Icon name="trash" /></button><button class="folder-button" title={t("openFolder")} onClick={(event) => { event.stopPropagation(); open(entry.sessionId); }}><Icon name="folder" /></button><button aria-expanded={isOpen} class="folder-button" title={t("meetingNotes")} onClick={(event) => { event.stopPropagation(); toggle(entry.sessionId); }}><Icon name="text" /></button></div></article>
      {isOpen && <MeetingNotesDetail analysis={analyses[entry.sessionId] ?? null} busy={analysisBusy === entry.sessionId} entry={entry} error={analysisErrors[entry.sessionId] ?? null} globalContext={globalContext} locale={locale} settings={settings} t={t} cancel={() => cancel(entry.sessionId)} generate={(content, options) => generate(entry.sessionId, content, options)} openSettings={openSettings} retry={() => retry(entry.sessionId)} saveContext={(content) => saveContext(entry.sessionId, content)} />}
    </div>; })}</div>}
  </div>;
}

export function SettingsView({ t, locale, settings, models, busy, modelTransitioning, saved, update, model, selectModel, installModel, cancelModel, chooseOutputDirectory, globalContext, globalNotice, globalRevision, saveGlobalContext, providerFetch, providerKeys, providerTest, updateGlobalContext }: ViewProps & {
  locale: OutputLanguage; settings: AppSettings; models: ModelCatalogEntry[]; busy: boolean; modelTransitioning: boolean; saved: boolean; update: (mutate: (next: AppSettings) => void) => void; model: ModelDownloadStatus; selectModel: (modelId: string) => void; installModel: () => void; cancelModel: () => void; chooseOutputDirectory: () => void;
  globalContext: GlobalContextContent; globalNotice: string | null; globalRevision: number; saveGlobalContext: () => void; providerFetch: ProviderFetchProps; providerKeys: ProviderKeyProps; providerTest: ProviderTestProps; updateGlobalContext: (content: GlobalContextContent) => void;
}) {
  const anyBlocked = providerKeys.blocked.meetingNotes || providerKeys.blocked.translation;
  const fetchBlocked = (blocked: boolean) => busy || blocked || providerTest.running !== null;
  const providerState = (provider: ProviderKind) => providerTestState({ dirty: providerDirty(settings, providerTest.confirmed, provider, providerKeys.secrets[provider]), failed: providerTest.errors[provider] !== null, testing: providerTest.running === provider, verified: settings[provider].verified });
  const providerAction = (provider: ProviderKind) => <ProviderTestButton disabled={busy || providerKeys.blocked[provider] || providerTest.running !== null} error={providerTest.errors[provider]} state={providerState(provider)} t={t} test={() => providerTest.test(provider)} />;
  const providerStatus = (provider: ProviderKind) => <ProviderErrorNote error={providerTest.errors[provider]} state={providerState(provider)} />;
  return <div class="settings-stack">
    <SettingsSection icon="mic" title={t("audio")}>
      <div class="form-grid"><RangeField label={t("microphoneGain")} value={settings.audio.microphoneGain} min={0} max={4} step={0.1} suffix="×" onChange={(value) => update((next) => { next.audio.microphoneGain = value; })} /><RangeField label={t("systemGain")} value={settings.audio.systemGain} min={0} max={4} step={0.1} suffix="×" onChange={(value) => update((next) => { next.audio.systemGain = value; })} />
      <SelectField label={t("format")} value={settings.audio.format} onChange={(value) => update((next) => { next.audio.format = value as AppSettings["audio"]["format"]; })} options={[{ value: "flac", label: "FLAC" }, { value: "wav", label: "WAV" }]} /><NumberField label={t("segmentMinutes")} value={settings.audio.segmentMinutes} min={1} max={1440} onChange={(value) => update((next) => { next.audio.segmentMinutes = value; })} /></div>
      <div class="path-field"><span>{t("output")}</span><div class="path-row"><code>{settings.audio.outputDirectory ?? t("appDefault")}</code><button class="icon-button" type="button" onClick={chooseOutputDirectory}><Icon name="folder" />{t("chooseFolder")}</button>{settings.audio.outputDirectory && <button class="icon-button" type="button" onClick={() => update((next) => { next.audio.outputDirectory = null; })}>{t("useDefault")}</button>}</div></div>
    </SettingsSection>

    <SettingsSection icon="archive" title={t("asr")} action={<ModelActionButton t={t} model={model} busy={modelTransitioning} install={installModel} cancel={cancelModel} />}>
      <div class="model-picker">
        <SelectField label={t("transcriptionModel")} value={settings.transcription.modelId} onChange={selectModel} options={models.map((item) => ({ value: item.id, label: item.displayName }))} />
      </div>
      <div class="form-grid">
        <SelectField label={t("recognitionLanguage")} value={settings.transcription.language} onChange={(value) => update((next) => { next.transcription.language = value; })} options={[{ value: "auto", label: t("automatic") }, { value: "zh-CN", label: t("chinese") }, { value: "en-US", label: t("english") }, { value: "Japanese", label: t("japanese") }]} />
        <NumberField label={t("threads")} value={settings.transcription.threads} min={1} max={16} onChange={(value) => update((next) => { next.transcription.threads = value; })} />
        <NumberField label={t("idleUnload")} value={settings.transcription.unloadAfterIdleMinutes} min={0} max={1440} onChange={(value) => update((next) => { next.transcription.unloadAfterIdleMinutes = value; })} />
        <RangeField label={t("vadThreshold")} value={settings.transcription.vad.activationThreshold} min={0.1} max={0.95} step={0.05} suffix="" onChange={(value) => update((next) => { next.transcription.vad.activationThreshold = value; })} />
      </div>
    </SettingsSection>

    <SettingsSection action={providerAction("translation")} icon="text" title={t("translation")}><Toggle label={t("enableTranslation")} checked={settings.translation.enabled} onChange={(checked) => update((next) => { next.translation.enabled = checked; })} /><div class="form-grid"><SelectField disabled={!settings.translation.enabled} label={t("translationLanguage")} value={settings.translation.targetLanguage} onChange={(value) => update((next) => { next.translation.targetLanguage = value; })} options={[{ value: "Chinese", label: t("chinese") }, { value: "English", label: t("english") }, { value: "Japanese", label: t("japanese") }, { value: "Korean", label: t("korean") }]} /><ProviderEndpointFields disabled={!settings.translation.enabled} endpoint={settings.translation.endpoint} endpointLabel={t("translationEndpoint")} t={t} onEndpoint={(value) => update((next) => { next.translation.endpoint = value; })} onSelect={(preset) => update((next) => { next.translation.endpoint = preset.endpoint; next.translation.model = preset.defaultModel; })} /><ProviderModelField disabled={!settings.translation.enabled} endpoint={settings.translation.endpoint} fetch={providerFetch} fetchBlocked={fetchBlocked(providerKeys.blocked.translation)} label={t("translationModel")} provider="translation" t={t} value={settings.translation.model} onChange={(value) => update((next) => { next.translation.model = value; })} /><ProviderKeyField configured={settings.translation.apiKeyConfigured} keys={providerKeys} provider="translation" t={t} /></div><EndpointWarning blocked={providerKeys.blocked.translation} endpoint={settings.translation.endpoint} t={t} />{providerStatus("translation")}</SettingsSection>

    <MeetingNotesProviderSection action={providerAction("meetingNotes")} blocked={providerKeys.blocked.meetingNotes} fetch={providerFetch} fetchBlocked={fetchBlocked(providerKeys.blocked.meetingNotes)} keys={providerKeys} settings={settings} status={providerStatus("meetingNotes")} t={t} update={update} />

    <GlobalContextSection busy={busy} content={globalContext} locale={locale} notice={globalNotice} revision={globalRevision} save={saveGlobalContext} t={t} onChange={updateGlobalContext} />

    <SettingsSection icon="gear" title={t("desktop")}><Toggle label={t("hideOnClose")} checked={settings.desktop.hideWindowOnClose} onChange={(checked) => update((next) => { next.desktop.hideWindowOnClose = checked; })} /><Toggle label={t("launchAtLogin")} checked={settings.desktop.launchAtLogin} onChange={(checked) => update((next) => { next.desktop.launchAtLogin = checked; })} /><Toggle label={t("recordOnLaunch")} checked={settings.desktop.startRecordingOnLaunch} onChange={(checked) => update((next) => { next.desktop.startRecordingOnLaunch = checked; })} /></SettingsSection>

    <SettingsSection icon="gear" title={t("appearance")}><div class="form-grid"><SelectField label={t("theme")} value={settings.desktop.theme} onChange={(value) => update((next) => { next.desktop.theme = value as AppSettings["desktop"]["theme"]; })} options={[{ value: "system", label: t("themeSystem") }, { value: "light", label: t("themeLight") }, { value: "dark", label: t("themeDark") }]} /><SelectField label={t("language")} value={settings.desktop.language} onChange={(value) => update((next) => { next.desktop.language = value as AppSettings["desktop"]["language"]; })} options={[{ value: "system", label: t("langSystem") }, { value: "zh-CN", label: t("langChinese") }, { value: "en-US", label: t("langEnglish") }]} /></div></SettingsSection>
    <div class={`save-bar ${anyBlocked || saved ? "active" : ""}`}>{anyBlocked && <p class="save-blocked">{t("insecureEndpointBlocked")}</p>}<p aria-live="polite" class="save-state">{saved && <><Icon name="check" />{t("saved")}</>}</p></div>
  </div>;
}

function ModelActionButton({ t, model, busy, install, cancel }: ViewProps & { model: ModelDownloadStatus; busy: boolean; install: () => void; cancel: () => void }) {
  const progress = model.totalBytes > 0 ? Math.min(100, model.downloadedBytes / model.totalBytes * 100) : 0;
  const downloaded = model.phase === "downloaded";
  const downloading = model.phase === "downloading";
  const label = downloaded
    ? t("modelReady")
    : downloading
      ? `${t("modelDownloading")} · ${Math.round(progress)}% · ${t("cancel")}`
      : `${t("downloadModel")} · ${formatBytes(model.totalBytes)}`;
  const title = downloading
    ? `${model.currentFile ?? t("modelFile")} · ${formatBytes(model.downloadedBytes)} / ${formatBytes(model.totalBytes)}`
    : label;
  return <button type="button" class={`model-action-button ${model.phase}`} disabled={downloaded || busy} onClick={downloading ? cancel : install} title={title} aria-live="polite">
    {downloading && <i class="model-action-progress" style={{ width: `${progress}%` }} />}
    <span class="model-action-content">
      <span class="model-action-icon" aria-hidden="true">
        <span class={`model-action-icon-state ${downloaded ? "" : "active"}`}><Icon name="download" /></span>
        <span class={`model-action-icon-state ready ${downloaded ? "active" : ""}`}><Icon name="check" /></span>
      </span>
      <span>{label}</span>
    </span>
  </button>;
}

function Level({ label, value, muted }: { label: string; value: number; muted: boolean }) {
  const normalizedValue = Math.max(-80, Math.min(0, value));
  const displayValue = muted ? "—" : `${Math.round(value)} dB`;
  const width = muted ? 0 : (normalizedValue + 80) / 80 * 100;
  return <div class={`level-row ${muted ? "muted" : ""}`}><span>{label}</span><div class="level-track" role="meter" aria-label={label} aria-valuemin={-80} aria-valuemax={0} aria-valuenow={muted ? undefined : Math.round(normalizedValue)} aria-valuetext={displayValue} aria-disabled={muted}><i style={{ width: `${width}%` }} /></div><em>{displayValue}</em></div>;
}

function sourceLabel(t: ReturnType<typeof translator>, source: HistoryEntry["audioSource"]) { return t(source); }
function segmentText(t: ReturnType<typeof translator>, segment: TranscriptSegment) {
  if (segment.status === "complete") return segment.text.trim();
  if (segment.status === "failed") return t("transcriptFailed");
  return `${t("transcribing")}…`;
}
function translationText(t: ReturnType<typeof translator>, translation: TranslationSegment) {
  if (translation.status === "complete") return translation.text.trim() || "…";
  if (translation.status === "failed") return t("translationFailed");
  return `${t("translating")}…`;
}
function translationLanguageLabel(t: ReturnType<typeof translator>, language: string) {
  const key = ({ Chinese: "chinese", English: "english", Japanese: "japanese", Korean: "korean" } as const)[language as "Chinese" | "English" | "Japanese" | "Korean"];
  return key ? t(key) : language;
}
// A snapshot supersedes another one for the same segment when it reflects a later
// transcription attempt, or a later stage of the same attempt.
const SEGMENT_STATUS_RANK: Record<TranscriptSegment["status"], number> = { pending: 0, processing: 1, complete: 2, failed: 2 };
function isNewerSegment(candidate: TranscriptSegment, existing: TranscriptSegment) {
  if (candidate.attempts !== existing.attempts) return candidate.attempts > existing.attempts;
  return SEGMENT_STATUS_RANK[candidate.status] >= SEGMENT_STATUS_RANK[existing.status];
}
function upsertSegment(segments: TranscriptSegment[], incoming: TranscriptSegment) {
  const index = segments.findIndex((segment) => segment.id === incoming.id);
  if (index < 0) return [...segments, incoming].sort((a, b) => a.id - b.id);
  if (!isNewerSegment(incoming, segments[index])) return segments;
  const next = [...segments];
  next[index] = incoming;
  return next;
}
function mergeSegments(fetched: TranscriptSegment[], received: TranscriptSegment[]) {
  const base = [...fetched].sort((a, b) => a.id - b.id);
  return received.reduce(upsertSegment, base);
}
const TRANSLATION_STATUS_RANK: Record<TranslationSegment["status"], number> = { pending: 0, processing: 1, complete: 2, failed: 2 };
function isNewerTranslation(candidate: TranslationSegment, existing: TranslationSegment) {
  if (candidate.attempts !== existing.attempts) return candidate.attempts > existing.attempts;
  return TRANSLATION_STATUS_RANK[candidate.status] >= TRANSLATION_STATUS_RANK[existing.status];
}
function upsertTranslation(translations: TranslationSegment[], incoming: TranslationSegment) {
  const index = translations.findIndex((translation) => translation.segmentId === incoming.segmentId);
  if (index < 0) return [...translations, incoming].sort((a, b) => a.segmentId - b.segmentId);
  if (!isNewerTranslation(incoming, translations[index])) return translations;
  const next = [...translations];
  next[index] = incoming;
  return next;
}
function mergeTranslations(fetched: TranslationSegment[], received: TranslationSegment[]) {
  const base = [...fetched].sort((a, b) => a.segmentId - b.segmentId);
  return received.reduce(upsertTranslation, base);
}

const root = document.getElementById("app");
if (root) render(<App />, root);
