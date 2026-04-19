#!/usr/bin/env python3
"""
Analyze Phase -1 JSONL run logs and compute go/no-go decision metrics.

Usage:
    python analyze_runs.py --runs-dir data/phase_minus1/runs [--output-md report.md]
"""

import argparse
import json
import sys
from pathlib import Path
from collections import defaultdict
from datetime import datetime
import statistics


def parse_args():
    parser = argparse.ArgumentParser(
        description="Analyze Phase -1 awareness POC run logs"
    )
    parser.add_argument(
        "--runs-dir",
        default="data/phase_minus1/runs",
        help="Directory containing JSONL run files (default: data/phase_minus1/runs)",
    )
    parser.add_argument(
        "--output-md",
        default=None,
        help="Optional markdown report output file",
    )
    return parser.parse_args()


def ansi_green(text):
    return f"\033[92m{text}\033[0m"


def ansi_red(text):
    return f"\033[91m{text}\033[0m"


def read_runs(runs_dir):
    """Read all .jsonl files from runs_dir. Skip malformed lines, log to stderr."""
    runs_dir = Path(runs_dir)
    if not runs_dir.exists():
        return []

    ticks = []
    jsonl_files = list(runs_dir.glob("*.jsonl"))

    if not jsonl_files:
        return []

    for filepath in jsonl_files:
        with open(filepath, "r") as f:
            for line_num, line in enumerate(f, 1):
                line = line.strip()
                if not line:
                    continue
                try:
                    tick = json.loads(line)
                    ticks.append(tick)
                except json.JSONDecodeError as e:
                    print(
                        f"Skipping malformed JSON in {filepath}:{line_num}: {e}",
                        file=sys.stderr,
                    )

    return ticks


def compute_metrics(ticks):
    """Compute all metrics from ticks."""
    if not ticks:
        return None

    # Basic counts
    total_ticks = len(ticks)

    # Parse timestamps for period calculation
    timestamps = []
    for tick in ticks:
        try:
            ts_str = tick.get("timestamp", "")
            # Parse ISO 8601 timestamp
            if ts_str:
                # Handle timezone: strip +/-HH:MM if present
                if "+" in ts_str:
                    ts_str = ts_str.split("+")[0]
                elif ts_str.count("-") > 2:  # More than ISO date dashes
                    parts = ts_str.rsplit("-", 1)
                    ts_str = parts[0]
                dt = datetime.fromisoformat(ts_str)
                timestamps.append(dt)
        except (ValueError, AttributeError):
            pass

    if timestamps:
        min_time = min(timestamps)
        max_time = max(timestamps)
        period_days = max(1, (max_time - min_time).days + 1)
        total_hours = max(1, (max_time - min_time).total_seconds() / 3600)
    else:
        period_days = 0
        total_hours = 1

    # Count alerts and ratings
    alerts = [t for t in ticks if t.get("api_response_raw", {}).get("should_alert")]
    total_alerts = len(alerts)

    # Rating breakdown for alerts only
    rating_counts = defaultdict(int)
    for alert in alerts:
        rating = alert.get("user_rating")
        if rating in ["useful", "not_useful", "annoying"]:
            rating_counts[rating] += 1
        else:  # null counts as not_useful
            rating_counts["not_useful"] += 1

    # Calculate useful_rate
    rated_alerts = sum(rating_counts.values())
    useful_rate = (
        (rating_counts["useful"] / rated_alerts * 100) if rated_alerts > 0 else 0
    )

    # Alerts per hour
    alerts_per_hour = total_alerts / total_hours if total_hours > 0 else 0

    # Cost
    total_cost = sum(t.get("api_cost_usd", 0) for t in ticks)
    daily_avg_cost = total_cost / period_days if period_days > 0 else 0

    # Alert type breakdown
    alert_types = defaultdict(int)
    for alert in alerts:
        alert_type = alert.get("api_response_raw", {}).get("alert_type", "unknown")
        alert_types[alert_type] += 1

    # Latency percentiles
    latencies = {"capture": [], "ocr": [], "api": []}
    for tick in ticks:
        elapsed = tick.get("elapsed_ms", {})
        if isinstance(elapsed, dict):
            if "capture" in elapsed:
                latencies["capture"].append(elapsed["capture"])
            if "ocr" in elapsed:
                latencies["ocr"].append(elapsed["ocr"])
            if "api" in elapsed:
                latencies["api"].append(elapsed["api"])

    median_latencies = {}
    for key in latencies:
        if latencies[key]:
            median_latencies[key] = statistics.median(latencies[key])
        else:
            median_latencies[key] = 0

    # Total latency calculation
    total_latencies = []
    for tick in ticks:
        elapsed = tick.get("elapsed_ms", {})
        if isinstance(elapsed, dict):
            total = elapsed.get("capture", 0) + elapsed.get("ocr", 0) + elapsed.get("api", 0)
            if total > 0:
                total_latencies.append(total)

    median_total_latency = (
        statistics.median(total_latencies) if total_latencies else 0
    )

    return {
        "total_ticks": total_ticks,
        "total_alerts": total_alerts,
        "rating_counts": dict(rating_counts),
        "useful_rate": useful_rate,
        "alerts_per_hour": alerts_per_hour,
        "total_cost": total_cost,
        "daily_avg_cost": daily_avg_cost,
        "period_days": period_days,
        "min_time": min_time if timestamps else None,
        "max_time": max_time if timestamps else None,
        "alert_types": dict(alert_types),
        "median_latencies": median_latencies,
        "median_total_latency": median_total_latency,
    }


def check_pass_fail(metrics):
    """Check go/no-go criteria."""
    passes = {
        "useful_rate": metrics["useful_rate"] >= 40.0,
        "alerts_per_hour": 2.0 <= metrics["alerts_per_hour"] <= 8.0,
        "daily_cost": metrics["daily_avg_cost"] < 0.30,
    }
    return passes


def format_timestamp(dt):
    """Format datetime as YYYY-MM-DD."""
    if dt is None:
        return "N/A"
    return dt.strftime("%Y-%m-%d")


def print_report(metrics):
    """Print metrics to stdout with ANSI colors."""
    if metrics is None:
        print("No ticks found", file=sys.stderr)
        return False

    min_time = metrics.get("min_time")
    max_time = metrics.get("max_time")

    print("\n=== Awareness POC — Phase -1 Analysis ===")
    print(
        f"Period: {format_timestamp(min_time)} to {format_timestamp(max_time)} ({metrics['period_days']} days)\n"
    )

    total_alerts = metrics["total_alerts"]
    alert_pct = (total_alerts / metrics["total_ticks"] * 100) if metrics["total_ticks"] > 0 else 0

    print(f"Total ticks:       {metrics['total_ticks']}")
    print(f"Ticks with alert:   {total_alerts}  ({alert_pct:.1f}%)")

    rating_counts = metrics["rating_counts"]
    total_ratings = sum(rating_counts.values())

    if total_alerts > 0:
        for rating in ["useful", "not_useful", "annoying"]:
            count = rating_counts.get(rating, 0)
            pct = (count / total_alerts * 100) if total_alerts > 0 else 0
            print(f"  {rating:<18} {count:>3}  ({pct:>5.1f}%)")
        # Unrated (null) line
        unrated = total_alerts - total_ratings
        pct = (unrated / total_alerts * 100) if total_alerts > 0 else 0
        print(f"  unrated (null):    {unrated:>3}  ({pct:>5.1f}%)")
    else:
        print("  useful:             0  (  0.0%)")
        print("  not_useful:         0  (  0.0%)")
        print("  annoying:           0  (  0.0%)")
        print("  unrated (null):     0  (  0.0%)")

    print()

    passes = check_pass_fail(metrics)

    useful_rate_str = f"{metrics['useful_rate']:.1f}%"
    useful_status = ansi_green("PASS") if passes["useful_rate"] else ansi_red("FAIL")
    print(f"useful_rate:       {useful_rate_str:<6} ← TARGET ≥ 40%  [{useful_status}]")

    alerts_per_hour_str = f"{metrics['alerts_per_hour']:.1f}"
    alerts_status = ansi_green("PASS") if passes["alerts_per_hour"] else ansi_red("FAIL")
    print(f"alerts_per_hour:    {alerts_per_hour_str:<6} ← TARGET 2-8    [{alerts_status}]")

    cost_str = f"${metrics['daily_avg_cost']:.2f}"
    cost_status = ansi_green("PASS") if passes["daily_cost"] else ansi_red("FAIL")
    print(f"cost_daily_avg:   {cost_str:<6} ← TARGET < $0.30 [{cost_status}]")

    print()
    print("Breakdown by alert_type:")
    total_for_pct = sum(metrics["alert_types"].values())
    for alert_type in sorted(metrics["alert_types"].keys()):
        count = metrics["alert_types"][alert_type]
        pct = (count / total_for_pct * 100) if total_for_pct > 0 else 0
        print(f"  {alert_type:<14} {count:>3} alerts ({pct:>2.0f}%)")

    print()
    print("Latency (median ms):")
    print(f"  capture: {metrics['median_latencies'].get('capture', 0):.0f}ms   "
          f"ocr: {metrics['median_latencies'].get('ocr', 0):.0f}ms   "
          f"api: {metrics['median_latencies'].get('api', 0):.0f}ms   "
          f"total: {metrics['median_total_latency']:.0f}ms")

    print()
    print(f"Total cost: ${metrics['total_cost']:.2f} over {metrics['period_days']} days")

    print()
    print("=== GO/NO-GO ===")
    print(
        f"useful_rate ≥ 40%:  {ansi_green('PASS') if passes['useful_rate'] else ansi_red('FAIL')} ({metrics['useful_rate']:.1f}%)"
    )
    print(
        f"alerts_per_hour 2-8: {ansi_green('PASS') if passes['alerts_per_hour'] else ansi_red('FAIL')} ({metrics['alerts_per_hour']:.1f})"
    )
    print(
        f"cost_daily_avg < $0.30: {ansi_green('PASS') if passes['daily_cost'] else ansi_red('FAIL')} (${metrics['daily_avg_cost']:.2f})"
    )

    all_pass = all(passes.values())
    result = "GO — advance to Phase 0 spikes" if all_pass else "NO-GO — iterate on Phase -1"
    result_str = ansi_green(result) if all_pass else ansi_red(result)
    print(f"→ RESULT: {result_str}\n")

    return all_pass


def generate_markdown(metrics):
    """Generate markdown report content (no ANSI colors)."""
    if metrics is None:
        return "# Awareness POC — Phase -1 Analysis\n\nNo ticks found.\n"

    min_time = metrics.get("min_time")
    max_time = metrics.get("max_time")

    md = []
    md.append("# Awareness POC — Phase -1 Analysis")
    md.append(
        f"\n**Period:** {format_timestamp(min_time)} to {format_timestamp(max_time)} ({metrics['period_days']} days)\n"
    )

    total_alerts = metrics["total_alerts"]
    alert_pct = (
        (total_alerts / metrics["total_ticks"] * 100)
        if metrics["total_ticks"] > 0
        else 0
    )

    md.append(f"**Total ticks:** {metrics['total_ticks']}")
    md.append(f"**Ticks with alert:** {total_alerts} ({alert_pct:.1f}%)\n")

    rating_counts = metrics["rating_counts"]
    total_ratings = sum(rating_counts.values())

    for rating in ["useful", "not_useful", "annoying"]:
        count = rating_counts.get(rating, 0)
        pct = (count / total_alerts * 100) if total_alerts > 0 else 0
        md.append(f"- **{rating}:** {count} ({pct:.1f}%)")

    unrated = total_alerts - total_ratings
    pct = (unrated / total_alerts * 100) if total_alerts > 0 else 0
    md.append(f"- **unrated (null):** {unrated} ({pct:.1f}%)\n")

    passes = check_pass_fail(metrics)

    md.append(f"**useful_rate:** {metrics['useful_rate']:.1f}% (TARGET ≥ 40%) — {'PASS' if passes['useful_rate'] else 'FAIL'}")
    md.append(
        f"**alerts_per_hour:** {metrics['alerts_per_hour']:.1f} (TARGET 2-8) — {'PASS' if passes['alerts_per_hour'] else 'FAIL'}"
    )
    md.append(
        f"**cost_daily_avg:** ${metrics['daily_avg_cost']:.2f} (TARGET < $0.30) — {'PASS' if passes['daily_cost'] else 'FAIL'}\n"
    )

    md.append("## Breakdown by alert_type\n")
    total_for_pct = sum(metrics["alert_types"].values())
    for alert_type in sorted(metrics["alert_types"].keys()):
        count = metrics["alert_types"][alert_type]
        pct = (count / total_for_pct * 100) if total_for_pct > 0 else 0
        md.append(f"- **{alert_type}:** {count} alerts ({pct:.0f}%)")

    md.append("\n## Latency (median ms)\n")
    md.append(
        f"- **capture:** {metrics['median_latencies'].get('capture', 0):.0f}ms\n"
    )
    md.append(f"- **ocr:** {metrics['median_latencies'].get('ocr', 0):.0f}ms\n")
    md.append(f"- **api:** {metrics['median_latencies'].get('api', 0):.0f}ms\n")
    md.append(
        f"- **total:** {metrics['median_total_latency']:.0f}ms\n"
    )

    md.append(
        f"\n**Total cost:** ${metrics['total_cost']:.2f} over {metrics['period_days']} days\n"
    )

    md.append("## GO/NO-GO Decision\n")
    all_pass = all(passes.values())
    result = "**GO** — advance to Phase 0 spikes" if all_pass else "**NO-GO** — iterate on Phase -1"
    md.append(f"- useful_rate ≥ 40%: {'PASS' if passes['useful_rate'] else 'FAIL'} ({metrics['useful_rate']:.1f}%)\n")
    md.append(f"- alerts_per_hour 2-8: {'PASS' if passes['alerts_per_hour'] else 'FAIL'} ({metrics['alerts_per_hour']:.1f})\n")
    md.append(f"- cost_daily_avg < $0.30: {'PASS' if passes['daily_cost'] else 'FAIL'} (${metrics['daily_avg_cost']:.2f})\n")
    md.append(f"\n**→ RESULT:** {result}\n")

    return "\n".join(md)


def main():
    args = parse_args()
    runs_dir = Path(args.runs_dir)

    # Check if runs directory exists
    if not runs_dir.exists():
        print(f"No run files found in {runs_dir}", file=sys.stderr)
        sys.exit(1)

    # Read JSONL files
    ticks = read_runs(runs_dir)

    if not ticks:
        print(f"No run files found in {runs_dir}", file=sys.stderr)
        sys.exit(1)

    # Compute metrics
    metrics = compute_metrics(ticks)

    if metrics is None:
        print("No ticks found", file=sys.stderr)
        sys.exit(1)

    # Print to stdout
    print_report(metrics)

    # Write markdown if requested
    if args.output_md:
        output_path = Path(args.output_md)
        markdown_content = generate_markdown(metrics)
        output_path.parent.mkdir(parents=True, exist_ok=True)
        with open(output_path, "w") as f:
            f.write(markdown_content)
        print(f"Markdown report written to {output_path}", file=sys.stderr)


if __name__ == "__main__":
    main()
