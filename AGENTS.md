# AGENTS.md

This file provides guidance to Codex (Codex.ai/code) when working with code in this repository.

## Project Overview

Resonote is a local-first audio recorder and offline voice transcription desktop app (Windows 10/11 and macOS 13+ only — `lib.rs` has a `compile_error!` for other targets). Built with Tauri 2: a minimal Preact frontend and a Rust backend that does all the real work. Privacy-first: zero telemetry, no cloud, ASR runs in-process via sherpa-onnx.

## Commands

```bash
npm install              # install frontend deps
npm run dev              # frontend-only preview at 127.0.0.1:1420 (no Rust backend; UI falls back to defaults via the isTauri check)
npm run tauri dev        # full app in development mode
npm run build            # tsc --noEmit + vite production build
npm run check:rust       # cargo check
npm run format:check     # cargo fmt --check
npm run lint             # cargo clippy --all-targets --all-features -- -D warnings
npm test                 # cargo test (all Rust unit tests)
npm run verify           # build + format:check + lint + test — run before completing a task or releasing
npm run bundle           # production packaging (tauri build)
```

Run a single Rust test:

```bash
cargo test --manifest-path src-tauri/Cargo.toml <test_name>
```

Device-dependent tests (need physical audio devices, ignored by default):

```bash
cargo test --manifest-path src-tauri/Cargo.toml <test_name> -- --ignored --nocapture
```

See docs/RELEASING.md for the list of these tests and the full release checklist.

## Architecture

Full details in docs/ARCHITECTURE.md. The essentials:

**Rust-centric core, thin frontend.** All audio capture, DSP, encoding, VAD, ASR, and file I/O live in Rust (`src-tauri/src/`). The Preact UI (`src/main.tsx`, single file) never touches raw PCM — it only receives status snapshots at 10 Hz via Tauri events: `recording-status`, `transcription-status`, `model-download-status`, plus per-sentence `transcript-segment` updates for the live transcript panel. All IPC commands are `#[tauri::command]` functions registered in `src-tauri/src/lib.rs`.

**Data flow:** capture (CPAL mic; WASAPI loopback on Windows / ScreenCaptureKit on macOS for system audio) → downmix + resample → gain/mix/limiter to 16 kHz mono f32 → forks into (a) streaming FLAC/WAV archive writer and (b) Silero VAD → speech segments enqueued in a persistent transcription queue → Qwen3-ASR via sherpa-onnx on a worker thread.

**Fail-safe by design:** recording and file writes are decoupled from transcription — transcription errors must never disrupt recording. Audio and `session.json` are checkpointed every 5 seconds with atomic writes; abnormal exits are recovered on startup. `transcript.json` tracks segment states (`pending`/`processing`/`complete`/`failed`); `processing` segments revert to `pending` after a crash, failed segments retry up to 3 times.

**Threading:** audio callbacks only copy samples to lock-free channels (no disk/model work); a recording loop thread does DSP/writing/VAD; a transcription service thread drains the queue; the ASR engine is loaded on demand and unloaded (RAII) after an idle timeout. Closing the window hides to tray; background services keep running.

**Key modules** (`src-tauri/src/`): `capture.rs` (device enumeration + capture), `audio/` (dsp, resample, flac, wav writers), `storage.rs` (session dirs, atomic JSON, recovery), `vad.rs`, `models.rs`/`asr.rs` (model download/validation, ASR engine), `transcription.rs` (queue), `recording.rs` (orchestration), `desktop.rs` (tray, single-instance, autostart), `settings.rs`, `history.rs`.

Some modules split large files with `#[path = "..."]` includes — e.g. `recording.rs` pulls in `recording_worker.rs`, `models.rs` pulls in `model_catalog.rs`/`model_files.rs`, and tests live in sibling `*_tests.rs` files included the same way.

**Frontend:** three files — `src/main.tsx` (entire UI), `src/types.ts` (mirrors Rust IPC types, camelCase via serde rename), `src/i18n.ts` (translations). No UI framework beyond Preact; keep it dependency-free. When adding/changing an IPC command or status struct, update `src/types.ts` to match the Rust serde output.

## Constraints & Gotchas

- **Version bumps** must keep three files in sync: `package.json`, `src-tauri/Cargo.toml`, `src-tauri/tauri.conf.json`. Then run `npm install --package-lock-only` and `cargo check` to refresh lockfiles.
- `windows-core` is pinned to `=0.61.2` in Cargo.toml to keep CPAL and Tauri on the same Windows ABI — don't bump it independently.
- **macOS deployment target**: the repo-root `.cargo/config.toml` pins `MACOSX_DEPLOYMENT_TARGET = "13.0"` to match `minimumSystemVersion` in tauri.conf.json; without it, bare `cargo test` binaries crash at launch (dyld can't resolve the Swift concurrency back-deployment @rpath). The file must stay at the repo root — cargo discovers config from the working directory, not from `--manifest-path`. Keep the version in sync with `minimumSystemVersion`.
- Resource policy: no background processes, no local HTTP servers, no heavy frontend dependencies. Inference stays in-process.
- `session.json` paths are strictly validated to prevent path traversal — preserve that validation when touching storage code.
- Clippy runs with `-D warnings`; new warnings fail `npm run verify`.
