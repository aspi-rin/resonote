import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { open } from "@tauri-apps/plugin-dialog";
import { render } from "preact";
import { useEffect, useMemo, useRef, useState } from "preact/hooks";
import { resolveLanguage, translator, type TranslationKey } from "./i18n";
import {
  DEFAULT_MODEL_STATUS,
  DEFAULT_RECORDING_STATUS,
  DEFAULT_SETTINGS,
  DEFAULT_TRANSCRIPTION_STATUS,
  type AppSettings,
  type AudioDevice,
  type HistoryEntry,
  type ModelDownloadStatus,
  type RecordingStatus,
  type TranscriptDocument,
  type TranscriptSegment,
  type TranscriptSegmentUpdate,
  type TranscriptionStatus,
} from "./types";
import "./styles.css";

type Tab = "record" | "history" | "settings";
type IconName =
  | "archive"
  | "check"
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
}

const isTauri = "__TAURI_INTERNALS__" in window;

function Icon({ name }: { name: IconName }) {
  const paths: Record<IconName, preact.JSX.Element> = {
    archive: <><path d="M4 7h16v13H4z"/><path d="M3 3h18v4H3zm6 8h6"/></>,
    check: <path d="m5 12 4 4L19 6"/>,
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
  const [devices, setDevices] = useState<AudioDevice[]>([]);
  const [recording, setRecording] = useState<RecordingStatus>(DEFAULT_RECORDING_STATUS);
  const [model, setModel] = useState<ModelDownloadStatus>(DEFAULT_MODEL_STATUS);
  const [transcription, setTranscription] = useState<TranscriptionStatus>(DEFAULT_TRANSCRIPTION_STATUS);
  const [liveTranscript, setLiveTranscript] = useState<LiveTranscript>({ segments: [], sessionId: null });
  const liveTranscriptSession = useRef<string | null>(null);
  const [history, setHistory] = useState<HistoryEntry[]>([]);
  const [version, setVersion] = useState("0.1.0");
  const [loading, setLoading] = useState(true);
  const [busy, setBusy] = useState(false);
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
      const [loadedSettings, loadedDevices, loadedRecording, loadedModel, loadedTranscription, loadedHistory, info] =
        await Promise.all([
          invoke<AppSettings>("get_settings"),
          invoke<AudioDevice[]>("list_audio_devices"),
          invoke<RecordingStatus>("get_recording_status"),
          invoke<ModelDownloadStatus>("get_model_status", { modelId: DEFAULT_SETTINGS.transcription.modelId }),
          invoke<TranscriptionStatus>("get_transcription_status"),
          invoke<HistoryEntry[]>("list_recording_history", { limit: 100 }),
          invoke<{ version: string }>("app_info"),
        ]);
      setSettings(loadedSettings);
      setDevices(loadedDevices);
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
      listen<ModelDownloadStatus>("model-download-status", (event) => setModel(event.payload)),
      listen<TranscriptionStatus>("transcription-status", (event) => setTranscription(event.payload)),
      listen<TranscriptSegmentUpdate>("transcript-segment", (event) => {
        setLiveTranscript((current) => {
          if (liveTranscriptSession.current !== event.payload.sessionId) return current;
          const segments = current.sessionId === event.payload.sessionId ? current.segments : [];
          return {
            segments: upsertSegment(segments, event.payload.segment),
            sessionId: event.payload.sessionId,
          };
        });
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
    setLiveTranscript((current) => (current.sessionId === sessionId ? current : { segments: [], sessionId }));
    if (!isTauri) return;
    // Seed from disk so a window reload or crash recovery does not lose earlier sentences.
    void invoke<TranscriptDocument | null>("get_session_transcript", { sessionId })
      .then((document) => {
        if (!document) return;
        setLiveTranscript((current) => {
          if (current.sessionId !== sessionId) return current;
          return { segments: mergeSegments(document.segments, current.segments), sessionId };
        });
      })
      .catch((reason) => console.warn("failed to seed live transcript", reason));
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

  const installModel = () => {
    if (!isTauri) {
      setModel({ ...model, phase: "downloaded", downloadedBytes: 1, totalBytes: 1 });
      return;
    }
    setError(null);
    void invoke<ModelDownloadStatus>("install_model", { modelId: settings.transcription.modelId })
      .then(setModel)
      .catch((reason) => setError(String(reason)));
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

        {tab === "record" && <RecordView t={t} settings={settings} recording={recording} transcription={transcription} model={model} liveSegments={liveTranscript.segments} busy={busy} isActive={isActive} update={update} start={startRecording} stop={stopRecording} installModel={installModel} cancelModel={() => void invoke("cancel_model_install")} />}
        {tab === "history" && <HistoryView t={t} locale={locale} history={history} refresh={() => void refreshHistory()} open={(sessionId) => void invoke("open_recording_directory", { sessionId }).catch((reason) => setError(String(reason)))} remove={(sessionId) => void deleteRecording(sessionId)} />}
        {tab === "settings" && <SettingsView t={t} settings={settings} devices={devices} busy={busy || isActive} saved={saved} update={update} save={() => void saveSettings()} model={model} installModel={installModel} chooseOutputDirectory={() => void chooseOutputDirectory()} />}
      </main>
    </div>
  );
}

function NavButton({ active, icon, label, onClick }: { active: boolean; icon: IconName; label: string; onClick: () => void }) {
  return <button class={`nav-button ${active ? "active" : ""}`} onClick={onClick}><Icon name={icon} /><span>{label}</span></button>;
}

interface ViewProps { t: ReturnType<typeof translator> }

function RecordView({ t, settings, recording, transcription, model, liveSegments, busy, isActive, update, start, stop, installModel, cancelModel }: ViewProps & {
  settings: AppSettings; recording: RecordingStatus; transcription: TranscriptionStatus; model: ModelDownloadStatus; liveSegments: TranscriptSegment[]; busy: boolean; isActive: boolean;
  update: (mutate: (next: AppSettings) => void) => void; start: () => void; stop: () => void; installModel: () => void; cancelModel: () => void;
}) {
  const progress = model.totalBytes > 0 ? Math.min(100, model.downloadedBytes / model.totalBytes * 100) : 0;
  return <div class="view-stack">
    <section class={`record-card ${isActive ? "active" : ""}`}>
      <div class="record-summary"><span class="record-label">{isActive ? t("recording") : t("ready")}</span><strong class="timer">{formatDuration(recording.elapsedMs)}</strong><span class="session-meta">{recording.segmentCount} {t("segments").toLowerCase()}</span></div>
      <Waveform microphone={recording.microphoneWaveform} system={recording.systemWaveform} active={isActive} />
      <button class={`record-button ${isActive ? "stop" : ""}`} disabled={busy} onClick={isActive ? stop : start}><span class="record-button-icon" />{isActive ? t("stop") : t("start")}</button>
    </section>

    <LiveTranscriptPanel t={t} segments={liveSegments} />

    <section class="quick-grid">
      <label class="field"><span>{t("source")}</span><select disabled={isActive} value={settings.audio.source} onChange={(event) => update((next) => { next.audio.source = event.currentTarget.value as AppSettings["audio"]["source"]; })}><option value="mixed">{t("mixed")}</option><option value="microphone">{t("microphone")}</option><option value="system">{t("system")}</option></select></label>
    </section>

    <section class="panel-card">
      <div class="section-heading"><div><span class="section-icon"><Icon name="wave" /></span><div><h2>{t("liveSignal")}</h2><p>{t("voiceActivity")}: {Math.round(recording.vadProbability * 100)}%</p></div></div></div>
      <Level label={t("microphone")} value={recording.microphoneDb} muted={settings.audio.source === "system"} />
      <Level label={t("system")} value={recording.systemDb} muted={settings.audio.source === "microphone"} />
    </section>

    <section class="panel-card model-card">
      <div class="section-heading"><div><span class="section-icon"><Icon name="archive" /></span><div><h2>{t("transcription")}</h2><p>{transcriptionLabel(t, transcription, model)}</p></div></div><span class={`model-dot ${model.phase}`} /></div>
      {model.phase === "downloading" ? <><div class="progress-track"><i style={{ width: `${progress}%` }} /></div><div class="progress-meta"><span>{model.currentFile ?? t("modelFile")}</span><span>{formatBytes(model.downloadedBytes)} / {formatBytes(model.totalBytes)}</span></div><button class="secondary-button" onClick={cancelModel}>{t("cancel")}</button></> : model.phase !== "downloaded" ? <button class="secondary-button accent" onClick={installModel}>{t("downloadModel")}</button> : <div class="model-ready"><Icon name="check" /><span>{t("modelReady")}</span>{transcription.pendingSegments > 0 && <em>{transcription.pendingSegments} {t("pendingSegments").toLowerCase()}</em>}</div>}
    </section>
  </div>;
}

const TRANSCRIPT_MIN_HEIGHT = 120;
const TRANSCRIPT_MAX_HEIGHT = 520;
const TRANSCRIPT_HEIGHT_STEP = 24;

function LiveTranscriptPanel({ t, segments }: ViewProps & { segments: TranscriptSegment[] }) {
  const [height, setHeight] = useState(220);
  const scrollRef = useRef<HTMLDivElement>(null);
  const pinnedToBottom = useRef(true);

  useEffect(() => {
    const scroller = scrollRef.current;
    if (scroller && pinnedToBottom.current) scroller.scrollTop = scroller.scrollHeight;
  }, [segments]);

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

  return <section class="panel-card transcript-panel">
    <div class="section-heading"><div><span class="section-icon"><Icon name="text" /></span><div><h2>{t("liveTranscript")}</h2><p>{t("liveTranscriptHint")}</p></div></div>{segments.length > 0 && <span class="transcript-count">{segments.length}</span>}</div>
    <div class="transcript-scroll" style={{ height: `${height}px` }} ref={scrollRef} onScroll={trackScroll} aria-live="polite" aria-relevant="additions text">
      {segments.length === 0
        ? <p class="transcript-empty">{t("liveTranscriptEmpty")}</p>
        : <ol class="transcript-list">{segments.map((segment) => <li class={`transcript-row ${segment.status}`} key={segment.id}><time>{formatDuration(segment.startMs)}</time><span>{segmentText(t, segment)}</span></li>)}</ol>}
    </div>
    <div class="transcript-resize" role="separator" aria-orientation="horizontal" aria-label={t("resizeTranscript")} aria-valuemin={TRANSCRIPT_MIN_HEIGHT} aria-valuemax={TRANSCRIPT_MAX_HEIGHT} aria-valuenow={height} tabIndex={0} onPointerDown={beginResize} onKeyDown={nudgeResize} />
  </section>;
}

function HistoryView({ t, locale, history, refresh, open, remove }: ViewProps & { locale: string; history: HistoryEntry[]; refresh: () => void; open: (id: string) => void; remove: (id: string) => void }) {
  return <div class="view-stack"><div class="view-actions"><p>{history.length} {t("history").toLowerCase()}</p><button class="icon-button" onClick={refresh}><Icon name="refresh" />{t("refresh")}</button></div>
    {history.length === 0 ? <section class="empty-state"><span><Icon name="history" /></span><h2>{t("noHistory")}</h2><p>{t("noHistoryHint")}</p></section> : <div class="history-list">{history.map((entry) => <article class="history-card" key={entry.sessionId}><div class="history-date"><strong>{new Date(entry.startedAt).toLocaleDateString(locale, { month: "short", day: "numeric" })}</strong><span>{new Date(entry.startedAt).toLocaleTimeString(locale, { hour: "2-digit", minute: "2-digit" })}</span></div><div class="history-body"><div class="history-title"><strong>{sourceLabel(t, entry.audioSource)}</strong><span>{formatDuration(entry.durationMs)} · {entry.audioFormat.toUpperCase()}</span></div><p>{entry.transcriptPreview || t("noTranscript")}</p><div class="history-tags"><span>{t(entry.status === "recording" ? "recording" : entry.status === "interrupted" ? "interrupted" : entry.status === "failed" ? "failed" : "completed")}</span>{entry.transcriptStatus && <span>{t(entry.transcriptStatus === "complete" ? "transcriptComplete" : entry.transcriptStatus === "partial" ? "transcriptPartial" : "transcriptPending")}</span>}</div></div><div class="history-actions"><button class="folder-button" title={t("openFolder")} onClick={() => open(entry.sessionId)}><Icon name="folder" /></button><button class="folder-button delete-button" disabled={entry.status === "recording"} title={t("deleteRecording")} onClick={() => remove(entry.sessionId)}><Icon name="trash" /></button></div></article>)}</div>}
  </div>;
}

function SettingsView({ t, settings, devices, busy, saved, update, save, model, installModel, chooseOutputDirectory }: ViewProps & { settings: AppSettings; devices: AudioDevice[]; busy: boolean; saved: boolean; update: (mutate: (next: AppSettings) => void) => void; save: () => void; model: ModelDownloadStatus; installModel: () => void; chooseOutputDirectory: () => void }) {
  const microphones = devices.filter((device) => device.source === "microphone" && !device.isDefault);
  const systems = devices.filter((device) => device.source === "system" && !device.isDefault);
  return <div class="settings-stack">
    <SettingsSection icon="mic" title={t("audio")}>
      <div class="form-grid"><SelectField label={t("microphoneDevice")} value={settings.audio.microphoneDeviceId ?? ""} onChange={(value) => update((next) => { next.audio.microphoneDeviceId = value || null; })} options={[{ value: "", label: t("defaultDevice") }, ...microphones.map((device) => ({ value: device.id, label: device.name }))]} /><SelectField label={t("systemDevice")} value={settings.audio.systemDeviceId ?? ""} onChange={(value) => update((next) => { next.audio.systemDeviceId = value || null; })} options={[{ value: "", label: t("defaultDevice") }, ...systems.map((device) => ({ value: device.id, label: device.name }))]} />
      <RangeField label={t("microphoneGain")} value={settings.audio.microphoneGain} min={0} max={4} step={0.1} suffix="×" onChange={(value) => update((next) => { next.audio.microphoneGain = value; })} /><RangeField label={t("systemGain")} value={settings.audio.systemGain} min={0} max={4} step={0.1} suffix="×" onChange={(value) => update((next) => { next.audio.systemGain = value; })} />
      <SelectField label={t("format")} value={settings.audio.format} onChange={(value) => update((next) => { next.audio.format = value as AppSettings["audio"]["format"]; })} options={[{ value: "flac", label: "FLAC" }, { value: "wav", label: "WAV" }]} /><NumberField label={t("segmentMinutes")} value={settings.audio.segmentMinutes} min={1} max={1440} onChange={(value) => update((next) => { next.audio.segmentMinutes = value; })} /></div>
      <div class="path-field"><span>{t("output")}</span><div class="path-row"><code>{settings.audio.outputDirectory ?? t("appDefault")}</code><button class="icon-button" type="button" onClick={chooseOutputDirectory}><Icon name="folder" />{t("chooseFolder")}</button>{settings.audio.outputDirectory && <button class="icon-button" type="button" onClick={() => update((next) => { next.audio.outputDirectory = null; })}>{t("useDefault")}</button>}</div></div>
    </SettingsSection>

    <SettingsSection icon="archive" title={t("asr")}><div class="form-grid"><SelectField label={t("recognitionLanguage")} value={settings.transcription.language} onChange={(value) => update((next) => { next.transcription.language = value; })} options={[{ value: "auto", label: t("automatic") }, { value: "zh-CN", label: t("chinese") }, { value: "en-US", label: t("english") }, { value: "Japanese", label: t("japanese") }]} /><NumberField label={t("threads")} value={settings.transcription.threads} min={1} max={16} onChange={(value) => update((next) => { next.transcription.threads = value; })} /><NumberField label={t("idleUnload")} value={settings.transcription.unloadAfterIdleMinutes} min={0} max={1440} onChange={(value) => update((next) => { next.transcription.unloadAfterIdleMinutes = value; })} /><RangeField label={t("vadThreshold")} value={settings.transcription.vad.activationThreshold} min={0.1} max={0.95} step={0.05} suffix="" onChange={(value) => update((next) => { next.transcription.vad.activationThreshold = value; })} /></div>{model.phase !== "downloaded" && <button class="secondary-button accent" onClick={installModel}>{t("downloadModel")}</button>}</SettingsSection>

    <SettingsSection icon="gear" title={t("desktop")}><Toggle label={t("hideOnClose")} checked={settings.desktop.hideWindowOnClose} onChange={(checked) => update((next) => { next.desktop.hideWindowOnClose = checked; })} /><Toggle label={t("launchAtLogin")} checked={settings.desktop.launchAtLogin} onChange={(checked) => update((next) => { next.desktop.launchAtLogin = checked; })} /><Toggle label={t("recordOnLaunch")} checked={settings.desktop.startRecordingOnLaunch} onChange={(checked) => update((next) => { next.desktop.startRecordingOnLaunch = checked; })} /></SettingsSection>

    <SettingsSection icon="gear" title={t("appearance")}><div class="form-grid"><SelectField label={t("theme")} value={settings.desktop.theme} onChange={(value) => update((next) => { next.desktop.theme = value as AppSettings["desktop"]["theme"]; })} options={[{ value: "system", label: t("themeSystem") }, { value: "light", label: t("themeLight") }, { value: "dark", label: t("themeDark") }]} /><SelectField label={t("language")} value={settings.desktop.language} onChange={(value) => update((next) => { next.desktop.language = value as AppSettings["desktop"]["language"]; })} options={[{ value: "system", label: t("langSystem") }, { value: "zh-CN", label: t("langChinese") }, { value: "en-US", label: t("langEnglish") }]} /></div></SettingsSection>
    <div class="save-bar"><button class="primary-button" disabled={busy} onClick={save}>{saved ? <><Icon name="check" />{t("saved")}</> : t("saveSettings")}</button></div>
  </div>;
}

function SettingsSection({ icon, title, children }: { icon: IconName; title: string; children: preact.ComponentChildren }) { return <section class="panel-card settings-section"><div class="section-heading"><div><span class="section-icon"><Icon name={icon} /></span><h2>{title}</h2></div></div>{children}</section>; }
function SelectField({ label, value, options, onChange }: { label: string; value: string; options: { value: string; label: string }[]; onChange: (value: string) => void }) { return <label class="field"><span>{label}</span><select value={value} onChange={(event) => onChange(event.currentTarget.value)}>{options.map((option) => <option value={option.value} key={option.value}>{option.label}</option>)}</select></label>; }
function NumberField({ label, value, min, max, onChange }: { label: string; value: number; min: number; max: number; onChange: (value: number) => void }) { return <label class="field"><span>{label}</span><input type="number" value={value} min={min} max={max} onChange={(event) => onChange(Number(event.currentTarget.value))} /></label>; }
function RangeField({ label, value, min, max, step, suffix, onChange }: { label: string; value: number; min: number; max: number; step: number; suffix: string; onChange: (value: number) => void }) { return <label class="range-field"><span><b>{label}</b><em>{value.toFixed(step < 0.1 ? 2 : 1)}{suffix}</em></span><input type="range" value={value} min={min} max={max} step={step} onInput={(event) => onChange(Number(event.currentTarget.value))} /></label>; }
function Toggle({ label, checked, onChange }: { label: string; checked: boolean; onChange: (checked: boolean) => void }) { return <label class="toggle-row"><span>{label}</span><input type="checkbox" checked={checked} onChange={(event) => onChange(event.currentTarget.checked)} /><i /></label>; }
function Level({ label, value, muted }: { label: string; value: number; muted: boolean }) { const width = muted ? 0 : Math.max(0, Math.min(100, (value + 80) / 80 * 100)); return <div class={`level-row ${muted ? "muted" : ""}`}><span>{label}</span><div class="level-track"><i style={{ width: `${width}%` }} /></div><em>{muted ? "—" : `${Math.round(value)} dB`}</em></div>; }
function Waveform({ microphone, system, active }: { microphone: number[]; system: number[]; active: boolean }) { const count = Math.max(microphone.length, system.length); return <div class={`waveform ${active ? "active" : ""}`} aria-hidden="true">{Array.from({ length: count }, (_, index) => <span class="waveform-column" key={index}><i class="waveform-microphone" style={{ height: `${Math.max(3, (microphone[index] ?? 0) * 88)}%` }} /><i class="waveform-system" style={{ height: `${Math.max(3, (system[index] ?? 0) * 88)}%` }} /></span>)}</div>; }

function sourceLabel(t: ReturnType<typeof translator>, source: HistoryEntry["audioSource"]) { return t(source); }
function segmentText(t: ReturnType<typeof translator>, segment: TranscriptSegment) {
  if (segment.status === "complete") return segment.text.trim() || "…";
  if (segment.status === "failed") return t("transcriptFailed");
  return `${t("transcribing")}…`;
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
function transcriptionLabel(t: ReturnType<typeof translator>, transcription: TranscriptionStatus, model: ModelDownloadStatus) { if (model.phase === "missing" || model.phase === "failed" || model.phase === "cancelled") return t("modelMissing"); if (model.phase === "downloading") return t("modelDownloading"); if (transcription.phase === "loadingModel") return t("modelLoading"); if (transcription.phase === "transcribing") return t("transcribing"); if (transcription.phase === "waitingForModel") return t("waitingModel"); return t("modelReady"); }
function formatDuration(milliseconds: number) { const total = Math.floor(milliseconds / 1000); const hours = Math.floor(total / 3600); const minutes = Math.floor(total % 3600 / 60); const seconds = total % 60; return `${String(hours).padStart(2, "0")}:${String(minutes).padStart(2, "0")}:${String(seconds).padStart(2, "0")}`; }
function formatBytes(bytes: number) { if (bytes <= 0) return "0 B"; const units = ["B", "KB", "MB", "GB"]; const index = Math.min(units.length - 1, Math.floor(Math.log(bytes) / Math.log(1024))); return `${(bytes / 1024 ** index).toFixed(index > 1 ? 1 : 0)} ${units[index]}`; }

render(<App />, document.getElementById("app")!);
