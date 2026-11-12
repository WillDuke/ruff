# /// script
# requires-python = ">=3.11"
# dependencies = []
# ///
"""Summarize the results of run_primer.py.

Usage: summarize.py RESULTS_JSONL [--markdown]
"""

import json
import statistics
import sys


def pct(numerator: float, denominator: float) -> float | None:
    return 100 * numerator / denominator if denominator else None


def fmt(value: float | None, suffix: str = "", digits: int = 1) -> str:
    return "–" if value is None else f"{value:.{digits}f}{suffix}"


def main() -> None:
    rows = [json.loads(line) for line in open(sys.argv[1]) if line.strip()]

    setup_failed = [r for r in rows if "error" in r]
    ran = [r for r in rows if "error" not in r]
    ty_ok = [r for r in ran if r["comparison"]["ty_files"] > 0]
    mypy_ok = [r for r in ran if r["comparison"]["mypy_files"] > 0]
    compared = [r for r in ran if r["comparison"]["matched"]["lines"] > 0]

    print(f"projects: {len(rows)} | setup failed: {len(setup_failed)} | "
          f"ty report: {len(ty_ok)} | mypy report: {len(mypy_ok)} | compared: {len(compared)}")

    # Performance of the report itself.
    overheads = [r["ty_report"]["seconds"] - r["ty_check"]["seconds"] for r in ty_ok]
    rel = [100 * (r["ty_report"]["seconds"] / r["ty_check"]["seconds"] - 1) for r in ty_ok
           if r["ty_check"]["seconds"] > 1]
    rss = [r["ty_report"]["rss_mb"] - r["ty_check"]["rss_mb"] for r in ty_ok]
    rss_rel = [100 * (r["ty_report"]["rss_mb"] / r["ty_check"]["rss_mb"] - 1) for r in ty_ok]
    print(f"ty report time overhead: median {statistics.median(overheads):.2f}s, "
          f"max {max(overheads):.2f}s; relative (checks over 1s) median {(statistics.median(rel) if rel else float("nan")):.0f}%")
    print(f"ty report memory overhead: median {statistics.median(rss):.0f} MB "
          f"({statistics.median(rss_rel):.0f}%), max {max(rss):.0f} MB")
    print(f"total time: ty check {sum(r['ty_check']['seconds'] for r in ty_ok):.0f}s, "
          f"ty check + report {sum(r['ty_report']['seconds'] for r in ty_ok):.0f}s, "
          f"mypy + report {sum(r['mypy_report']['seconds'] for r in mypy_ok):.0f}s")

    # Agreement over all compared lines.
    m = {k: sum(r["comparison"]["matched"][k] for r in compared)
         for k in compared[0]["comparison"]["matched"]} if compared else {}
    if m:
        print(f"compared: {m['files']} files, {m['lines']} lines; imprecision ty "
              f"{pct(m['ty_flagged'], m['lines']):.2f}% vs mypy {pct(m['mypy_flagged'], m['lines']):.2f}%")
        print(f"agreement (flagged or not): {pct(m['agree'], m['lines']):.2f}%; exact category: "
              f"{pct(m['agree_exact'], m['lines']):.2f}%; only ty flags: {pct(m['ty_only'], m['lines']):.2f}%, "
              f"only mypy flags: {pct(m['mypy_only'], m['lines']):.2f}%")
        per_project = [pct(r["comparison"]["matched"]["agree"], r["comparison"]["matched"]["lines"])
                       for r in compared]
        print(f"per-project agreement: median {statistics.median(per_project):.1f}%, "
              f"min {min(per_project):.1f}%")

    if compared:
        from collections import Counter

        ty_reasons: Counter = Counter()
        mypy_reasons: Counter = Counter()
        for r in compared:
            ty_reasons.update(dict(r["comparison"].get("ty_only_reasons", [])))
            mypy_reasons.update(dict(r["comparison"].get("mypy_only_reasons", [])))
        print("top reasons for lines only ty flags (ty's type):")
        for reason, n in ty_reasons.most_common(15):
            print(f"  {n:6} {reason}")
        print("top reasons for lines only mypy flags (mypy's kind of Any):")
        for reason, n in mypy_reasons.most_common(10):
            print(f"  {n:6} {reason}")

    if "--markdown" in sys.argv:
        print()
        print("| Project | ty check | + report | Δ RSS | mypy + report | mypy RSS | ty imprecision | "
              "mypy imprecision | compared lines | agreement |")
        print("|---|---|---|---|---|---|---|---|---|---|")
        for r in sorted(ran, key=lambda r: r["name"].lower()):
            c = r["comparison"]
            mt = c["matched"]
            mypy_ran = c["mypy_files"] > 0
            print(
                f"| {r['name']} | {fmt(r['ty_check']['seconds'], 's', 2)} | "
                f"{fmt(r['ty_report']['seconds'], 's', 2)} | "
                f"{fmt(r['ty_report']['rss_mb'] - r['ty_check']['rss_mb'], ' MB', 0)} | "
                f"{fmt(r['mypy_report']['seconds'], 's', 1) if mypy_ran else 'failed'} | "
                f"{fmt(r['mypy_report']['rss_mb'], ' MB', 0) if mypy_ran else '–'} | "
                f"{fmt(pct(c['ty_total']['flagged'], c['ty_total']['lines']), '%')} | "
                f"{fmt(pct(c['mypy_total']['flagged'], c['mypy_total']['lines']), '%')} | "
                f"{mt['lines']} | {fmt(pct(mt['agree'], mt['lines']), '%')} |"
            )
        for r in setup_failed:
            print(f"| {r['name']} | setup failed | | | | | | | | |")


if __name__ == "__main__":
    main()
