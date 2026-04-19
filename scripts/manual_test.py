#!/usr/bin/env python3
"""
Phase -1 validation script for awareness-poc.
Validates whether proactive context-based feedback has product value,
before any Rust code is written.
"""

import argparse
import json
import os
import select
import signal
import subprocess
import sys
import time
from collections import deque
from datetime import datetime, timezone, timedelta
from io import BytesIO
from pathlib import Path

try:
    from dotenv import load_dotenv
except ImportError:
    print("python-dotenv not installed. Run: pip install python-dotenv", file=sys.stderr)
    sys.exit(1)

try:
    import openai
except ImportError:
    print("openai not installed. Run: pip install openai", file=sys.stderr)
    sys.exit(1)

try:
    from PIL import Image
except ImportError:
    print("Pillow not installed. Run: pip install pillow", file=sys.stderr)
    sys.exit(1)

try:
    import pytesseract
except ImportError:
    print("pytesseract not installed. Run: pip install pytesseract", file=sys.stderr)
    sys.exit(1)


# ── Cost constants ──────────────────────────────────────────────────────────
INPUT_COST_PER_M  = 0.15   # USD per 1M input tokens
OUTPUT_COST_PER_M = 0.60   # USD per 1M output tokens

# ── System prompt ───────────────────────────────────────────────────────────
SYSTEM_PROMPT = """\
És o observador de ecrã de uma app de awareness pessoal.
Recebes texto extraído por OCR do ecrã do utilizador.

Não sabes nada sobre o utilizador — os seus hábitos, horários, preferências ou contexto de vida.
Não tens baseline. Não assumes que trabalho é melhor que lazer, nem o contrário.
Não julgas o que o utilizador está a fazer.

O teu único objectivo: alertar APENAS quando vês algo factualmente anómalo no próprio ecrã —
ou seja, um sinal claro de problema activo, visível no texto recebido.

Exemplos de alertas válidos:
- Mensagem de erro visível que o utilizador pode não ter reparado
- O mesmo conteúdo sem qualquer mudança durante vários ticks seguidos (bloqueio?)
- Contradição óbvia no ecrã: reunião a começar visível mas está noutra app
- Texto que indica frustração explícita ("not working", "erro", "por que não funciona")

Exemplos de NÃO alertar:
- Utilizador no YouTube, redes sociais, jogos — não sabes se é pausa, lazer ou trabalho
- Qualquer julgamento sobre produtividade sem evidência concreta no ecrã
- Dúvida — se não tens sinal claro, não alertas

Devolve SEMPRE JSON válido neste formato:
{
  "should_alert": boolean,
  "alert_type": "error_visible" | "stuck" | "contradiction" | "frustration" | "none",
  "urgency": "low" | "medium" | "high",
  "needs_deep_analysis": boolean,
  "quick_message": string,
  "screen_summary": string | null
}

quick_message: máximo 15 palavras, em PT, só se should_alert for true. Caso contrário string vazia.
screen_summary: 1-2 frases descrevendo o que está no ecrã, só se should_alert for true. Caso contrário null.
Em caso de dúvida: should_alert = false.\
"""

# ── Colour helpers ──────────────────────────────────────────────────────────
RED    = "\033[91m"
YELLOW = "\033[93m"
GREEN  = "\033[92m"
RESET  = "\033[0m"
BOLD   = "\033[1m"


def red(s: str) -> str:    return f"{RED}{s}{RESET}"
def yellow(s: str) -> str: return f"{YELLOW}{s}{RESET}"
def green(s: str) -> str:  return f"{GREEN}{s}{RESET}"
def bold(s: str) -> str:   return f"{BOLD}{s}{RESET}"


# ── Time helpers ────────────────────────────────────────────────────────────
def time_of_day(dt: datetime) -> str:
    h = dt.hour
    if 6  <= h < 12: return "morning"
    if 12 <= h < 18: return "afternoon"
    if 18 <= h < 22: return "evening"
    return "night"


def relative_minutes(past: datetime, now: datetime) -> str:
    delta = int((now - past).total_seconds() / 60)
    return f"-{delta}m"


# ── Screenshot ──────────────────────────────────────────────────────────────
def take_screenshot(output_dir: Path) -> tuple[str, Image.Image]:
    shots_dir = output_dir / "shots"
    shots_dir.mkdir(parents=True, exist_ok=True)
    ts = datetime.now().strftime("%Y%m%dT%H%M%S")
    path = shots_dir / f"{ts}.png"

    # Env with XWayland display for tools that need it
    env_x11 = {**os.environ, "DISPLAY": os.environ.get("DISPLAY", ":0")}
    tmp_path = "/tmp/awareness_shot.png"

    # Try grim (wlroots compositors: Sway, Hyprland)
    try:
        result = subprocess.run(["grim", "-"], capture_output=True, timeout=10)
        if result.returncode == 0 and result.stdout:
            img = Image.open(BytesIO(result.stdout))
            img.save(str(path))
            return str(path), img
    except (FileNotFoundError, subprocess.TimeoutExpired, Exception):
        pass

    # Try gnome-screenshot via XWayland (GNOME/Mutter)
    try:
        result = subprocess.run(
            ["gnome-screenshot", "-f", tmp_path],
            capture_output=True, timeout=15, env=env_x11,
        )
        if result.returncode == 0 and os.path.exists(tmp_path):
            img = Image.open(tmp_path)
            img.save(str(path))
            return str(path), img
    except (FileNotFoundError, subprocess.TimeoutExpired, Exception):
        pass

    # Try scrot via XWayland
    try:
        result = subprocess.run(
            ["scrot", tmp_path],
            capture_output=True, timeout=10, env=env_x11,
        )
        if result.returncode == 0 and os.path.exists(tmp_path):
            img = Image.open(tmp_path)
            img.save(str(path))
            return str(path), img
    except (FileNotFoundError, subprocess.TimeoutExpired, Exception):
        pass

    raise RuntimeError(
        "No screenshot tool available. Install gnome-screenshot (GNOME) or grim (wlroots)."
    )


# ── OCR ─────────────────────────────────────────────────────────────────────
def extract_text(image: Image.Image) -> str:
    raw = pytesseract.image_to_string(image, lang="por+eng")
    # Collapse excessive whitespace
    lines = [ln.strip() for ln in raw.splitlines() if ln.strip()]
    text = " ".join(lines)
    return text[:3000]


# ── OpenAI call ─────────────────────────────────────────────────────────────
def call_openai(client: openai.OpenAI, context_payload: dict) -> tuple[dict, int, int]:
    """Returns (parsed_response, input_tokens, output_tokens)."""
    response = client.chat.completions.create(
        model="gpt-4o-mini",
        max_tokens=150,
        temperature=0.3,
        messages=[
            {"role": "system", "content": SYSTEM_PROMPT},
            {"role": "user", "content": json.dumps(context_payload)},
        ],
        response_format={"type": "json_object"},
    )
    raw_text = response.choices[0].message.content or ""
    input_tokens  = response.usage.prompt_tokens if response.usage else 0
    output_tokens = response.usage.completion_tokens if response.usage else 0

    try:
        parsed = json.loads(raw_text)
    except json.JSONDecodeError:
        print(f"  [warn] Failed to parse API JSON: {raw_text[:200]}", file=sys.stderr)
        parsed = None

    return parsed, input_tokens, output_tokens


def mock_response() -> tuple[dict, int, int]:
    return (
        {
            "should_alert": True,
            "alert_type": "focus",
            "urgency": "medium",
            "needs_deep_analysis": False,
            "quick_message": "DRY RUN \u2014 teste de alerta",
        },
        0,
        0,
    )


# ── Rating prompt ────────────────────────────────────────────────────────────
def ask_rating() -> str | None:
    """Non-blocking stdin read with 30s timeout. Returns rating string or None."""
    sys.stdout.write("Rating [u=useful / n=not_useful / a=annoying / <enter>=skip]: ")
    sys.stdout.flush()
    ready, _, _ = select.select([sys.stdin], [], [], 30)
    if ready:
        line = sys.stdin.readline().strip().lower()
        mapping = {"u": "useful", "n": "not_useful", "a": "annoying"}
        return mapping.get(line, None)
    print()  # newline after timeout
    return None


# ── Main loop ────────────────────────────────────────────────────────────────
def main() -> None:
    parser = argparse.ArgumentParser(description="Awareness-PoC Phase -1 manual test")
    parser.add_argument("--interval-seconds", type=int, default=120)
    parser.add_argument("--budget-usd", type=float, default=0.50)
    parser.add_argument("--output-dir", type=str, default="data/phase_minus1")
    parser.add_argument("--annotate", action="store_true",
                        help="Ask for optional note before each API call")
    parser.add_argument("--dry-run", action="store_true",
                        help="Skip real API call, use mock response")
    args = parser.parse_args()

    # Load .env — try script dir, then repo root
    script_dir = Path(__file__).resolve().parent
    repo_root  = script_dir.parent
    for env_path in [script_dir / ".env", repo_root / ".env"]:
        if env_path.exists():
            load_dotenv(dotenv_path=env_path)
            break
    else:
        load_dotenv()  # let python-dotenv search default locations

    if not args.dry_run:
        api_key = os.environ.get("OPENAI_API_KEY", "").strip()
        if not api_key:
            print(red("OPENAI_API_KEY not set. Add it to .env or export it."), file=sys.stderr)
            sys.exit(1)
        client = openai.OpenAI(api_key=api_key)
    else:
        client = None  # not used in dry-run

    output_dir = Path(args.output_dir)
    (output_dir / "shots").mkdir(parents=True, exist_ok=True)
    (output_dir / "runs").mkdir(parents=True, exist_ok=True)

    today_str  = datetime.now().strftime("%Y-%m-%d")
    jsonl_path = output_dir / "runs" / f"{today_str}.jsonl"

    # ── State ────────────────────────────────────────────────────────────────
    history: deque[dict] = deque(maxlen=5)
    cumulative_cost = 0.0
    tick_id         = 0
    alerts_shown    = 0
    rating_counts: dict[str, int] = {}

    # ── SIGINT handler ───────────────────────────────────────────────────────
    def handle_sigint(sig, frame):
        print()
        print(bold("\n=== Session summary ==="))
        print(f"  Ticks     : {tick_id}")
        print(f"  Alerts    : {alerts_shown}")
        print(f"  Cost      : ${cumulative_cost:.4f}")
        if rating_counts:
            for k, v in sorted(rating_counts.items()):
                print(f"  {k:12s}: {v}")
        print("Bye.")
        sys.exit(0)

    signal.signal(signal.SIGINT, handle_sigint)

    print(bold(f"awareness-poc Phase -1 | interval={args.interval_seconds}s | "
               f"budget=${args.budget_usd:.2f} | dry-run={args.dry_run}"))
    print(f"Output: {jsonl_path}")
    print()

    while True:
        tick_id += 1
        now_local = datetime.now()
        now_utc   = datetime.now(timezone.utc)
        budget_left = args.budget_usd - cumulative_cost

        print(bold(f"[tick {tick_id} | {now_local.strftime('%H:%M:%S')} | "
                   f"budget left: ${budget_left:.4f}]"))

        # ── Screenshot ───────────────────────────────────────────────────────
        t0 = time.monotonic()
        try:
            shot_path, img = take_screenshot(output_dir)
        except RuntimeError as exc:
            print(red(f"  Screenshot failed: {exc}"))
            time.sleep(args.interval_seconds)
            continue
        capture_ms = int((time.monotonic() - t0) * 1000)

        # ── OCR ──────────────────────────────────────────────────────────────
        t1 = time.monotonic()
        ocr_text = extract_text(img)
        ocr_ms   = int((time.monotonic() - t1) * 1000)
        ocr_chars = len(ocr_text)
        print(f"  OCR: {ocr_chars} chars ({ocr_ms}ms)")
        if ocr_text:
            preview = ocr_text[:300].replace("\n", " ")
            print(f"  OCR preview: {preview}")

        # ── Optional annotation ──────────────────────────────────────────────
        user_note_before = ""
        if args.annotate:
            sys.stdout.write("  Note before API call (optional, <enter>=skip): ")
            sys.stdout.flush()
            ready, _, _ = select.select([sys.stdin], [], [], 30)
            if ready:
                user_note_before = sys.stdin.readline().strip()

        # ── Build context payload ─────────────────────────────────────────────
        history_list = [
            {
                "t": relative_minutes(
                    datetime.fromisoformat(entry["ts"]).replace(tzinfo=None),
                    now_local,
                ),
                "screen_excerpt": entry["screen_excerpt"],
            }
            for entry in history
        ]

        context_payload = {
            "timestamp": now_utc.isoformat(),
            "screen_text_excerpt": ocr_text,
            "user_note": user_note_before,
            "history_last_30min": history_list,
            "time_of_day": time_of_day(now_local),
        }

        # ── API call ─────────────────────────────────────────────────────────
        t2 = time.monotonic()
        if args.dry_run:
            parsed, input_tokens, output_tokens = mock_response()
        else:
            parsed, input_tokens, output_tokens = call_openai(client, context_payload)
        api_ms = int((time.monotonic() - t2) * 1000)

        tick_cost = (
            input_tokens  * INPUT_COST_PER_M  / 1_000_000 +
            output_tokens * OUTPUT_COST_PER_M / 1_000_000
        )
        cumulative_cost += tick_cost
        print(f"  API: {input_tokens}in/{output_tokens}out tokens | "
              f"${tick_cost:.5f} | {api_ms}ms")
        if parsed:
            print(f"  API response: {json.dumps(parsed, ensure_ascii=False)}")

        # ── Alert display ─────────────────────────────────────────────────────
        user_rating     = None
        user_note_after = None

        if parsed and parsed.get("should_alert"):
            alerts_shown += 1
            alert_type   = parsed.get("alert_type", "").upper()
            message      = parsed.get("quick_message", "")
            t_str        = now_local.strftime("%H:%M")
            print("\a", end="", flush=True)  # bell
            print(yellow(f"[{t_str}] {alert_type}: {message}"))
            summary = parsed.get("screen_summary")
            if summary:
                print(f"  → {summary}")
            user_rating = ask_rating()
            if user_rating:
                rating_counts[user_rating] = rating_counts.get(user_rating, 0) + 1

            sys.stdout.write("  Note after alert (optional, <enter>=skip): ")
            sys.stdout.flush()
            ready, _, _ = select.select([sys.stdin], [], [], 15)
            if ready:
                user_note_after = sys.stdin.readline().strip() or None
        elif parsed:
            print(f"  No alert ({parsed.get('alert_type', 'none')}, {parsed.get('urgency', '')}) "
                  f"— \"{parsed.get('quick_message', '')}\"")

        else:
            print("  API returned unparseable response — skipping alert.")

        # ── Write JSONL ───────────────────────────────────────────────────────
        record = {
            "tick_id":               tick_id,
            "timestamp":             now_utc.isoformat(),
            "screenshot_path":       shot_path,
            "ocr_chars":             ocr_chars,
            "ocr_truncated_excerpt": ocr_text[:200],
            "api_request_tokens":    input_tokens,
            "api_response_tokens":   output_tokens,
            "api_cost_usd":          tick_cost,
            "api_response_raw":      parsed if parsed else {},
            "user_rating":           user_rating,
            "user_note_after":       user_note_after,
            "elapsed_ms": {
                "capture": capture_ms,
                "ocr":     ocr_ms,
                "api":     api_ms,
            },
        }

        with open(jsonl_path, "a", encoding="utf-8") as fh:
            fh.write(json.dumps(record, ensure_ascii=False) + "\n")

        # ── Update history ────────────────────────────────────────────────────
        history.append({
            "ts":             now_local.isoformat(),
            "screen_excerpt": ocr_text[:200],
        })

        # ── Budget check ──────────────────────────────────────────────────────
        if cumulative_cost >= args.budget_usd:
            print(red(f"\nBudget exhausted (${cumulative_cost:.4f} >= "
                      f"${args.budget_usd:.2f}). Exiting."))
            sys.exit(2)

        print(f"  Sleeping {args.interval_seconds}s …\n")
        time.sleep(args.interval_seconds)


if __name__ == "__main__":
    main()
