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
