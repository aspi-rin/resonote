# Resonote

Resonote is a secure, local-first audio recorder and voice transcription tool for Windows and macOS, built with Tauri and sherpa-onnx.

## Key Features

- **Local Recording**: Captures system-default microphone audio, system audio, or a mix of both, with live source switching during recording.
- **Offline Transcription**: Powered by Silero VAD with selectable Qwen3-ASR 0.6B and 1.7B models via `sherpa-onnx`, running entirely on your machine.
- **Robust Storage**: Saves audio in streaming FLAC or recoverable WAV format, with auto-segmentation (1–1440 min) and auto-recovery after abnormal exits.
- **Privacy First**: Zero telemetry, zero cloud uploads. The speech recognition pipeline is completely local and runs in-process.
- **Cross-Platform**: Support for dark/light themes, system tray, launch on startup, and responsive layout.

## Supported Platforms

| Platform | Mic | System Audio | Notes |
| --- | --- | --- | --- |
| Windows 10/11 | WASAPI | WASAPI Loopback | Pre-built sherpa-onnx supports x64 |
| macOS 13+ | CoreAudio | ScreenCaptureKit | Requires Mic & Screen Recording permissions (audio only) |

## Quick Start

1. **Configure**: Open **Settings** to choose the output directory, audio format, and transcription model.
2. **Download Model**: Download the selected offline ASR model from the **Transcription model** settings section.
3. **Record**: Go to **Record** and click **Start**. The audio source can be switched while recording; hardware follows the operating system defaults.
4. **History**: View transcripts, manage recordings, or open output folders in **History**.

## File Structure

Recordings are organized under `year/month/day/Session_ID`:

```text
2026/07/19/20260719T.../
├── session.json              # Recording metadata and segments
├── audio-0000.flac           # Main recording (or .wav)
├── transcript.json           # Transcribed text
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

- **100% Offline**: All audio and transcripts remain on your local machine.
- **Safe Downloads**: Models are checked against SHA-256 hashes and securely extracted.
- **Sandboxed WebView**: Uses minimal Tauri capabilities to restrict unauthorized file system or network access.
