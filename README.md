# Resonote

Resonote is a secure, local-first audio recorder and voice transcription tool for Windows and macOS, built with Tauri and sherpa-onnx.

## Key Features

- **Local Recording**: Captures system-default microphone audio, system audio, or a mix of both, with live source switching during recording.
- **Offline Transcription**: Powered by Silero VAD with selectable Qwen3-ASR 0.6B and 1.7B models via `sherpa-onnx`, running entirely on your machine.
- **Local Translation**: Translates each completed sentence through a configurable OpenAI-compatible endpoint. The default is `http://127.0.0.1:8000/v1` with `Hy-MT2-1.8B`.
- **Meeting Notes** (optional): Cleans up a finished transcript and writes a structured summary through its own OpenAI-compatible endpoint, guided by reusable global context and per-meeting background notes.
- **Robust Storage**: Saves audio in streaming FLAC or recoverable WAV format, with auto-segmentation (1–1440 min) and auto-recovery after abnormal exits.
- **Privacy First**: Zero telemetry. Audio and speech recognition stay local; only translation and meeting notes send text to the endpoints you configure.
- **Cross-Platform**: Support for dark/light themes, system tray, launch on startup, and responsive layout.

## Supported Platforms

| Platform | Mic | System Audio | Notes |
| --- | --- | --- | --- |
| Windows 10/11 | WASAPI | WASAPI Loopback | Pre-built sherpa-onnx supports x64 |
| macOS 13+ | CoreAudio | ScreenCaptureKit | Requires Mic & Screen Recording permissions (audio only) |

## Quick Start

1. **Configure**: Open **Settings** to choose the output directory, audio format, transcription model, and translation language or endpoint.
2. **Download Model**: Download the selected offline ASR model from the **Transcription model** settings section.
3. **Record**: Go to **Record** and click **Start**. The audio source can be switched while recording; real-time transcripts and translations appear in the live panel.
4. **History**: View transcripts and translations, generate meeting notes for a finished recording, manage recordings, or open output folders in **History**.

## File Structure

Recordings are organized under `year/month/day/Session_ID`:

```text
2026/07/19/20260719T.../
├── session.json              # Recording metadata and segments
├── audio-0000.flac           # Main recording (or .wav)
├── transcript.json           # Transcribed text
├── translation.json          # Translated text and retry state
├── analysis.json             # Meeting context, notes and generation state
└── speech/
    ├── speech-000001.wav     # Segmented voice clips for ASR
    └── speech-000002.wav
```

## Development

### Prerequisites
- Node.js 22+
- Rust 1.87+
- [Tauri 2 Prerequisites](https://v2.tauri.app/start/prerequisites/)

### Commands
```bash
# Install dependencies
npm install

# Run frontend preview only (with mock data)
npm run dev

# Run application in development mode
npm run tauri dev

# Build production bundle
npm run bundle
```

For architecture details, see [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md). For release checklists, see [docs/RELEASING.md](docs/RELEASING.md).

## Privacy & Security

- **Local by Default**: Recording, ASR, and every audio file stay on the machine. Both configurable endpoints default to loopback.
- **What Leaves the Machine**: Translation sends one transcript sentence per request. Meeting notes send the transcript text plus your global and meeting context. Nothing else is transmitted — audio is never uploaded, and both features are used only against the endpoints you configure in Settings.
- **API Keys**: Each endpoint takes an optional API key, stored locally in `settings.json` and bound to the endpoint it was saved for. The Settings form shows the key for its own endpoint so you can check or reveal it; keys are never written into recording folders or logged.
- **Safe Downloads**: Models are checked against SHA-256 hashes and securely extracted.
- **Sandboxed WebView**: Uses minimal Tauri capabilities to restrict unauthorized file system or network access.
