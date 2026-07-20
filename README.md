# Resonote

Resonote is a secure, local-first audio recorder and voice transcription tool for Windows and macOS, built with Tauri and sherpa-onnx.

## Key Features

- **Local Recording**: Captures microphone, system audio, or a mix of both.
- **Offline Transcription**: Powered by Silero VAD and Qwen3-ASR (0.6B INT8) via `sherpa-onnx` running entirely on your machine.
- **Robust Storage**: Saves audio in streaming FLAC or recoverable WAV format, with auto-segmentation (1–1440 min) and auto-recovery after abnormal exits.
- **Privacy First**: Zero telemetry, zero cloud uploads. The speech recognition pipeline is completely local and runs in-process.
- **Cross-Platform**: Support for dark/light themes, system tray, launch on startup, and responsive layout.

## Supported Platforms

| Platform | Mic | System Audio | Notes |
| --- | --- | --- | --- |
| Windows 10/11 | WASAPI | WASAPI Loopback | Pre-built sherpa-onnx supports x64 |
| macOS 13+ | CoreAudio | ScreenCaptureKit | Requires Mic & Screen Recording permissions (audio only) |

## Quick Start

1. **Configure**: Open **Settings** to choose audio sources, directory, and formats.
2. **Download Model**: Click **Download Model** to download the offline ASR model (~840 MB).
3. **Record**: Go to **Record** and click **Start**. You will see real-time waveforms and voice activity indicators.
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
