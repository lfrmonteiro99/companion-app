# Awareness POC

Validate proactive context-based feedback before building the product.

## Quick start

```bash
# Verify environment
bash scripts/check_env.sh

# Run Phase -1 (Python validation)
python scripts/manual_test.py --dry-run

# Run Phase 1+ (Rust CLI)
cargo run -- run
```

## Phases

- **Phase -1**: Python manual validation and spike
- **Phase 0**: Technical spikes (screenshot, OCR, context extraction)
- **Phase 1-4**: Rust CLI implementation with feedback loops
- **Phase 5**: Dogfooding and evaluation

## Requirements

- Ubuntu 24.04+
- GNOME desktop environment
- Wayland session
- PipeWire audio

## Setup

Copy `.env.example` to `.env` and fill in your `OPENAI_API_KEY`:

```bash
cp .env.example .env
# Edit .env and add your key
```

See [EVAL_PROTOCOL.md](./EVAL_PROTOCOL.md) for dogfooding metrics and go/no-go criteria.

## Android

A sibling Android scaffold lives in [`android/`](./android). It's a Kotlin +
Jetpack Compose app that reuses the cross-platform Rust logic from
`crates/awareness-cli` via a JNI wrapper in `android/core-rs`.

Platform split:

| Concern          | Linux (`crates/awareness-cli`)     | Android (`android/`)                         |
|------------------|-------------------------------------|----------------------------------------------|
| Screen capture   | xdg-desktop-portal + PipeWire       | `MediaProjection` + `VirtualDisplay`         |
| OCR              | Tesseract (`leptess`)               | ML Kit Text Recognition                      |
| Audio capture    | `cpal`                              | `AudioRecord`                                |
| STT              | `whisper-rs`                        | TBD (whisper.cpp JNI / system recognizer)    |
| Gating + API     | Rust (shared via core-rs)           | Rust (shared via core-rs)                    |

Build:

```bash
# 1. Build the Rust core for Android ABIs (requires Android NDK + cargo-ndk)
cd android/core-rs && ./build.sh

# 2. Open `android/` in Android Studio, or:
cd android && ./gradlew :app:assembleDebug
```
