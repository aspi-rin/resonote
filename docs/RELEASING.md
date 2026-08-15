# Release Checklist

Resonote must be built and packaged on their respective target operating systems. Signing and compiling ScreenCaptureKit code for macOS requires Xcode and the macOS SDK.

## 1. Pre-Release Verification

Run the following commands in a clean workspace:

```bash
npm ci
npm run verify
git status --short
```

`npm run verify` runs TypeScript validation, Vite production build, Rust formatting checks, Clippy, Rust unit tests, and the frontend Vitest suite.

### Device-Dependent Tests
The following tests require physical audio devices and are ignored by default. Run them manually:

```bash
cargo test --manifest-path src-tauri/Cargo.toml opens_default_system_loopback_stream -- --ignored --nocapture
cargo test --manifest-path src-tauri/Cargo.toml records_default_system_loopback_to_disk -- --ignored --nocapture
cargo test --manifest-path src-tauri/Cargo.toml records_default_mixed_sources_to_disk -- --ignored --nocapture
cargo test --manifest-path src-tauri/Cargo.toml switches_default_sources_while_recording -- --ignored --nocapture
```
*Note: Ensure audio is playing and input devices are not locked by other applications before running.*

### Manual QA Checklist
- **Permissions**: Granting, denying, and resetting audio/screen recording permissions.
- **Recording**: Test mic, system audio, and mixed sources for at least 10 minutes.
- **Stability**: Record for 2+ hours to verify memory and CPU stability.
- **Formats**: Test FLAC, WAV, and custom directory outputs.
- **Recovery**: Kill the app process while recording; ensure audio is repaired and marked as "recovered" upon restart.
- **Transcription**: Model downloading, pause/resume, offline transcription, and idle unloading.
- **Desktop integration**: Tray actions, close-to-tray, single-instance behavior, and auto-start on login.

## 2. Version Bump

Ensure the version string is identical in the following files:
- `package.json` -> `version`
- `src-tauri/Cargo.toml` -> `version`
- `src-tauri/tauri.conf.json` -> `version`

Run `npm install --package-lock-only` and `cargo check --manifest-path src-tauri/Cargo.toml` to update lock files. Commit lockfile changes directly.

## 3. Packaging

Generate installation packages:

```bash
# Package for all formats on the current OS
npm run bundle

# Package specific formats
# Windows:
npm run tauri build -- --bundles nsis
npm run tauri build -- --bundles msi

# macOS:
npm run tauri build -- --bundles app,dmg
```

*Note: Bundled outputs are stored in `src-tauri/target/release/bundle/` (ignored by Git).*

## 4. Signing

- **Windows**: Use a valid Code Signing Certificate configured via Tauri's `windows.signCommand` or a CI pipeline. Never commit signing credentials.
- **macOS**: Sign with a Developer ID Application certificate, enable hardened runtime, and notarize via Apple.

## 5. Auto-Updater

Resonote has auto-update disabled by default. Enabling it requires:
1. A secure HTTPS update manifest endpoint and file hosting.
2. Generating a Tauri updater keypair (keep the private key offline or in secure CI secrets).
3. Thorough testing for network failures, malformed manifests, invalid signatures, and fallback behavior.

## 6. Git Tagging

Resonote can be completely managed offline. Once the release commits are finalized, create a local annotated tag:

```bash
git tag -a v0.1.0 -m "Resonote v0.1.0"
git show --stat v0.1.0
```
