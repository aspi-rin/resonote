import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { open } from "@tauri-apps/plugin-dialog";
import { render } from "preact";
import { useEffect, useMemo, useRef, useState } from "preact/hooks";
import { resolveLanguage, translator, type TranslationKey } from "./i18n";
import packageMetadata from "../package.json";
import {
  DEFAULT_MODEL_STATUS,
  DEFAULT_RECORDING_STATUS,
  DEFAULT_SETTINGS,
  DEFAULT_TRANSCRIPTION_STATUS,
  type AppSettings,
  type AudioSourceMode,
  type HistoryEntry,
  type ModelCatalogEntry,
  type ModelDownloadStatus,
  type RecordingStatus,
  type TranscriptDocument,
  type TranscriptSegment,
  type TranscriptSegmentUpdate,
  type TranscriptionStatus,
  type TranslationDocument,
  type TranslationSegment,
  type TranslationSegmentUpdate,
} from "./types";
import "./styles.css";

type Tab = "record" | "history" | "settings";
type IconName =
  | "archive"
  | "check"
  | "download"
  | "folder"
  | "gear"
  | "history"
  | "mic"
  | "refresh"
  | "shield"
  | "text"
  | "trash"
  | "wave";

interface LiveTranscript {
  segments: TranscriptSegment[];
  sessionId: string | null;
  translations: TranslationSegment[];
}

const isTauri = "__TAURI_INTERNALS__" in window;
const FALLBACK_MODEL_CATALOG: ModelCatalogEntry[] = [
  { displayName: "Qwen3-ASR 0.6B INT8 · Multilingual", id: "qwen3-asr-0.6b-int8", totalBytes: 879_346_277 },
  { displayName: "Qwen3-ASR 1.7B INT8 · High accuracy", id: "qwen3-asr-1.7b-int8", totalBytes: 2_404_866_275 },
];

function Icon({ name }: { name: IconName }) {
  const paths: Record<IconName, preact.JSX.Element> = {
    archive: <><path d="M4 7h16v13H4z"/><path d="M3 3h18v4H3zm6 8h6"/></>,
    check: <path d="m5 12 4 4L19 6"/>,
    download: <><path d="M12 3v12m-5-5 5 5 5-5"/><path d="M5 20h14"/></>,
    folder: <path d="M3 6h7l2 2h9v11H3z"/>,
    gear: <><circle cx="12" cy="12" r="3"/><path d="M19 13.5v-3l-2-.7-.7-1.7.9-1.9-2.1-2.1-1.9.9-1.7-.7L10.5 2h-3l-.7 2-1.7.7-1.9-.9-2.1 2.1.9 1.9-.7 1.7-2 .7v3l2 .7.7 1.7-.9 1.9 2.1 2.1 1.9-.9 1.7.7.7 2h3l.7-2 1.7-.7 1.9.9 2.1-2.1-.9-1.9.7-1.7z"/></>,
    history: <><path d="M3 12a9 9 0 1 0 3-6.7L3 8"/><path d="M3 3v5h5m4-2v6l4 2"/></>,
    mic: <><rect x="8" y="3" width="8" height="12" rx="4"/><path d="M5 11a7 7 0 0 0 14 0m-7 7v3m-4 0h8"/></>,
    refresh: <><path d="M20 7v5h-5"/><path d="M19 12a7 7 0 1 0-2 5"/></>,
    shield: <path d="M12 2 4 5v6c0 5 3.4 8.6 8 11 4.6-2.4 8-6 8-11V5z"/>,
    text: <path d="M4 6h16M4 11h16M4 16h10"/>,
    trash: <><path d="M4 7h16m-10 4v6m4-6v6M9 7l1-3h4l1 3m3 0-1 14H7L6 7"/></>,
    wave: <path d="M3 12h2l2-7 3 14 3-11 2 8 2-4h4"/>,
  };
  return <svg class="icon" viewBox="0 0 24 24" aria-hidden="true">{paths[name]}</svg>;
}

function App() {
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
  const t = useMemo(() => translator(settings.desktop.language), [settings.desktop.language]);
  const locale = resolveLanguage(settings.desktop.language);

  const refreshHistory = async () => {
    if (!isTauri) return;
    setHistory(await invoke<HistoryEntry[]>("list_recording_history", { limit: 100 }));
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
      const [loadedRecording, loadedModels, loadedModel, loadedTranscription, loadedHistory, info] =
        await Promise.all([
          invoke<RecordingStatus>("get_recording_status"),
          invoke<ModelCatalogEntry[]>("list_transcription_models"),
          invoke<ModelDownloadStatus>("get_model_status", { modelId: loadedSettings.transcription.modelId }),
          invoke<TranscriptionStatus>("get_transcription_status"),
          invoke<HistoryEntry[]>("list_recording_history", { limit: 100 }),
          invoke<{ version: string }>("app_info"),
        ]);
      setSettings(loadedSettings);
      setModels(loadedModels);
      liveTranscriptSession.current = loadedRecording.sessionId;
      setRecording(loadedRecording);
      setModel(loadedModel);
      setTranscription(loadedTranscription);
      setHistory(loadedHistory);
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

  const update = (mutate: (next: AppSettings) => void) => {
    setSaved(false);
    setSettings((current) => {
      const next = structuredClone(current);
      mutate(next);
      return next;
    });
  };

  const saveSettings = async () => {
    setBusy(true);
    setError(null);
    try {
      const persisted = isTauri ? await invoke<AppSettings>("save_settings", { settings }) : settings;
      setSettings(persisted);
      setSaved(true);
      window.setTimeout(() => setSaved(false), 1800);
    } catch (reason) {
      setError(String(reason));
    } finally {
      setBusy(false);
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
      const persisted = await invoke<AppSettings>("save_settings", { settings });
      setSettings(persisted);
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
        setSettings(await invoke<AppSettings>("save_settings", { settings: nextSettings }));
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
        setSettings(await invoke<AppSettings>("save_settings", { settings: nextSettings }));
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
        <div class="privacy-note"><Icon name="shield" /><div><strong>{t("localOnly")}</strong><p>{t("localOnlyHint")}</p></div></div>
        <span class="version">{t("version")} {version}</span>
      </aside>

      <main class="main-panel">
        <header class="topbar">
          <div><p class="eyebrow">RESONOTE / {t(tab)}</p><h1>{t(tab)}</h1></div>
          <span class={`phase-badge phase-${recording.phase}`}><i />{phaseLabel}</span>
        </header>

        {error && <div class="error-banner" role="alert"><strong>{t("error")}</strong><span>{error}</span><button onClick={() => setError(null)}>×</button></div>}

        {tab === "record" && <RecordView t={t} settings={settings} recording={recording} transcription={transcription} model={model} liveSegments={liveTranscript.segments} liveTranslations={liveTranscript.translations} busy={busy} isActive={isActive} changeLanguages={changeRecordingLanguages} changeSource={changeAudioSource} start={startRecording} stop={stopRecording} />}
        {tab === "history" && <HistoryView t={t} locale={locale} history={history} refresh={() => void refreshHistory()} open={(sessionId) => void invoke("open_recording_directory", { sessionId }).catch((reason) => setError(String(reason)))} remove={(sessionId) => void deleteRecording(sessionId)} />}
        {tab === "settings" && <SettingsView t={t} settings={settings} models={models} busy={busy || isActive} modelTransitioning={modelTransitioning} saved={saved} update={update} save={() => void saveSettings()} model={model} selectModel={(modelId) => void selectModel(modelId)} installModel={installModel} cancelModel={() => void cancelModel()} chooseOutputDirectory={() => void chooseOutputDirectory()} />}
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
        : <ol class="transcript-list">{segments.map((segment) => { const translation = translations.find((item) => item.segmentId === segment.id); return <li class={`transcript-row ${segment.status}`} key={segment.id}><time>{formatDuration(segment.startMs)}</time><div class="transcript-copy"><span class="transcript-source">{segmentText(t, segment)}</span>{translation && <span class={`transcript-translation ${translation.status}`}>{translationText(t, translation)}</span>}</div></li>; })}</ol>}
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

function HistoryView({ t, locale, history, refresh, open, remove }: ViewProps & { locale: string; history: HistoryEntry[]; refresh: () => void; open: (id: string) => void; remove: (id: string) => void }) {
  return <div class="view-stack"><div class="view-actions"><p>{history.length} {t("history").toLowerCase()}</p><button class="icon-button" onClick={refresh}><Icon name="refresh" />{t("refresh")}</button></div>
    {history.length === 0 ? <section class="empty-state"><span><Icon name="history" /></span><h2>{t("noHistory")}</h2><p>{t("noHistoryHint")}</p></section> : <div class="history-list">{history.map((entry) => <article class="history-card" key={entry.sessionId}><div class="history-date"><strong>{new Date(entry.startedAt).toLocaleDateString(locale, { month: "short", day: "numeric" })}</strong><span>{new Date(entry.startedAt).toLocaleTimeString(locale, { hour: "2-digit", minute: "2-digit" })}</span></div><div class="history-body"><div class="history-title"><strong>{sourceLabel(t, entry.audioSource)}</strong><span>{formatDuration(entry.durationMs)} · {entry.audioFormat.toUpperCase()}</span></div><p class="history-transcript">{entry.transcriptPreview || t("noTranscript")}</p>{entry.translationStatus && <p class="history-translation"><strong>{entry.translationTargetLanguage ? `${t("translateTo")} ${translationLanguageLabel(t, entry.translationTargetLanguage)}` : t("translation")}</strong><span>{entry.translationPreview || (entry.translationStatus === "partial" ? t("translationFailed") : t("translating"))}</span></p>}<div class="history-tags"><span>{t(entry.status === "recording" ? "recording" : entry.status === "interrupted" ? "interrupted" : entry.status === "failed" ? "failed" : "completed")}</span>{entry.transcriptStatus && <span>{t(entry.transcriptStatus === "complete" ? "transcriptComplete" : entry.transcriptStatus === "partial" ? "transcriptPartial" : "transcriptPending")}</span>}{entry.translationStatus && <span>{t(entry.translationStatus === "complete" ? "translationComplete" : entry.translationStatus === "partial" ? "translationPartial" : "translationPending")}</span>}</div></div><div class="history-actions"><button class="folder-button" title={t("openFolder")} onClick={() => open(entry.sessionId)}><Icon name="folder" /></button><button class="folder-button delete-button" disabled={entry.status === "recording"} title={t("deleteRecording")} onClick={() => remove(entry.sessionId)}><Icon name="trash" /></button></div></article>)}</div>}
  </div>;
}

function SettingsView({ t, settings, models, busy, modelTransitioning, saved, update, save, model, selectModel, installModel, cancelModel, chooseOutputDirectory }: ViewProps & { settings: AppSettings; models: ModelCatalogEntry[]; busy: boolean; modelTransitioning: boolean; saved: boolean; update: (mutate: (next: AppSettings) => void) => void; save: () => void; model: ModelDownloadStatus; selectModel: (modelId: string) => void; installModel: () => void; cancelModel: () => void; chooseOutputDirectory: () => void }) {
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

    <SettingsSection icon="text" title={t("translation")}><Toggle label={t("enableTranslation")} checked={settings.translation.enabled} onChange={(checked) => update((next) => { next.translation.enabled = checked; })} /><div class="form-grid"><SelectField disabled={!settings.translation.enabled} label={t("translationLanguage")} value={settings.translation.targetLanguage} onChange={(value) => update((next) => { next.translation.targetLanguage = value; })} options={[{ value: "Chinese", label: t("chinese") }, { value: "English", label: t("english") }, { value: "Japanese", label: t("japanese") }, { value: "Korean", label: t("korean") }]} /><TextField disabled={!settings.translation.enabled} label={t("translationModel")} value={settings.translation.model} onChange={(value) => update((next) => { next.translation.model = value; })} /><TextField className="field-wide" disabled={!settings.translation.enabled} label={t("translationEndpoint")} type="url" value={settings.translation.endpoint} onChange={(value) => update((next) => { next.translation.endpoint = value; })} /></div></SettingsSection>

    <SettingsSection icon="gear" title={t("desktop")}><Toggle label={t("hideOnClose")} checked={settings.desktop.hideWindowOnClose} onChange={(checked) => update((next) => { next.desktop.hideWindowOnClose = checked; })} /><Toggle label={t("launchAtLogin")} checked={settings.desktop.launchAtLogin} onChange={(checked) => update((next) => { next.desktop.launchAtLogin = checked; })} /><Toggle label={t("recordOnLaunch")} checked={settings.desktop.startRecordingOnLaunch} onChange={(checked) => update((next) => { next.desktop.startRecordingOnLaunch = checked; })} /></SettingsSection>

    <SettingsSection icon="gear" title={t("appearance")}><div class="form-grid"><SelectField label={t("theme")} value={settings.desktop.theme} onChange={(value) => update((next) => { next.desktop.theme = value as AppSettings["desktop"]["theme"]; })} options={[{ value: "system", label: t("themeSystem") }, { value: "light", label: t("themeLight") }, { value: "dark", label: t("themeDark") }]} /><SelectField label={t("language")} value={settings.desktop.language} onChange={(value) => update((next) => { next.desktop.language = value as AppSettings["desktop"]["language"]; })} options={[{ value: "system", label: t("langSystem") }, { value: "zh-CN", label: t("langChinese") }, { value: "en-US", label: t("langEnglish") }]} /></div></SettingsSection>
    <div class="save-bar"><button class="primary-button" disabled={busy} onClick={save}>{saved ? <><Icon name="check" />{t("saved")}</> : t("saveSettings")}</button></div>
  </div>;
}

function SettingsSection({ icon, title, action, children }: { icon: IconName; title: string; action?: preact.ComponentChildren; children: preact.ComponentChildren }) { return <section class="panel-card settings-section"><div class={`section-heading ${action ? "has-action" : ""}`}><div><span class="section-icon"><Icon name={icon} /></span><h2>{title}</h2></div>{action}</div>{children}</section>; }
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

function SelectField({ label, value, options, disabled = false, onChange }: { label: string; value: string; options: { value: string; label: string }[]; disabled?: boolean; onChange: (value: string) => void }) { return <label class="field"><span>{label}</span><select value={value} disabled={disabled} onChange={(event) => onChange(event.currentTarget.value)}>{options.map((option) => <option value={option.value} key={option.value}>{option.label}</option>)}</select></label>; }
function TextField({ label, value, onChange, type = "text", disabled = false, className = "" }: { label: string; value: string; onChange: (value: string) => void; type?: "text" | "url"; disabled?: boolean; className?: string }) { return <label class={`field ${className}`}><span>{label}</span><input disabled={disabled} type={type} value={value} onInput={(event) => onChange(event.currentTarget.value)} /></label>; }
function NumberField({ label, value, min, max, onChange }: { label: string; value: number; min: number; max: number; onChange: (value: number) => void }) { return <label class="field"><span>{label}</span><input type="number" value={value} min={min} max={max} onChange={(event) => onChange(Number(event.currentTarget.value))} /></label>; }
function RangeField({ label, value, min, max, step, suffix, onChange }: { label: string; value: number; min: number; max: number; step: number; suffix: string; onChange: (value: number) => void }) { return <label class="range-field"><span><b>{label}</b><em>{value.toFixed(step < 0.1 ? 2 : 1)}{suffix}</em></span><input type="range" value={value} min={min} max={max} step={step} onInput={(event) => onChange(Number(event.currentTarget.value))} /></label>; }
function Toggle({ label, checked, onChange }: { label: string; checked: boolean; onChange: (checked: boolean) => void }) { return <label class="toggle-row"><span>{label}</span><input type="checkbox" checked={checked} onChange={(event) => onChange(event.currentTarget.checked)} /><i /></label>; }
function Level({ label, value, muted }: { label: string; value: number; muted: boolean }) {
  const normalizedValue = Math.max(-80, Math.min(0, value));
  const displayValue = muted ? "—" : `${Math.round(value)} dB`;
  const width = muted ? 0 : (normalizedValue + 80) / 80 * 100;
  return <div class={`level-row ${muted ? "muted" : ""}`}><span>{label}</span><div class="level-track" role="meter" aria-label={label} aria-valuemin={-80} aria-valuemax={0} aria-valuenow={muted ? undefined : Math.round(normalizedValue)} aria-valuetext={displayValue} aria-disabled={muted}><i style={{ width: `${width}%` }} /></div><em>{displayValue}</em></div>;
}

function sourceLabel(t: ReturnType<typeof translator>, source: HistoryEntry["audioSource"]) { return t(source); }
function segmentText(t: ReturnType<typeof translator>, segment: TranscriptSegment) {
  if (segment.status === "complete") return segment.text.trim() || "…";
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
function formatDuration(milliseconds: number) { const total = Math.floor(milliseconds / 1000); const hours = Math.floor(total / 3600); const minutes = Math.floor(total % 3600 / 60); const seconds = total % 60; return `${String(hours).padStart(2, "0")}:${String(minutes).padStart(2, "0")}:${String(seconds).padStart(2, "0")}`; }
function formatBytes(bytes: number) { if (bytes <= 0) return "0 B"; const units = ["B", "KB", "MB", "GB"]; const index = Math.min(units.length - 1, Math.floor(Math.log(bytes) / Math.log(1024))); return `${(bytes / 1024 ** index).toFixed(index > 1 ? 1 : 0)} ${units[index]}`; }

render(<App />, document.getElementById("app")!);
