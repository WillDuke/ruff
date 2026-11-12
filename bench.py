# /// script
# requires-python = ">=3.11"
# dependencies = []
# ///
"""Time `ty check` with and without `--html-report` and record peak memory.

Usage: bench.py TY_BINARY PROJECT_DIR [RUNS]

Each measurement runs in a fresh Python process so that `ru_maxrss` of the
children reflects that single ty invocation.
"""

import json
import shutil
import statistics
import subprocess
import sys
from pathlib import Path

MEASURE = """
import json, resource, subprocess, sys, time
start = time.perf_counter()
proc = subprocess.run(sys.argv[2:], cwd=sys.argv[1], stdout=subprocess.DEVNULL, stderr=subprocess.PIPE, text=True)
elapsed = time.perf_counter() - start
rss_kb = resource.getrusage(resource.RUSAGE_CHILDREN).ru_maxrss
print(json.dumps({"seconds": elapsed, "rss_mb": rss_kb / 1024, "code": proc.returncode, "stderr": proc.stderr[-500:]}))
"""


def measure(cwd: Path, cmd: list[str]) -> dict:
    out = subprocess.run(
        [sys.executable, "-c", MEASURE, str(cwd), *cmd],
        check=True,
        capture_output=True,
        text=True,
    )
    return json.loads(out.stdout)


def main() -> None:
    ty, project = sys.argv[1], Path(sys.argv[2]).resolve()
    runs = int(sys.argv[3]) if len(sys.argv) > 3 else 3
    report_dir = project.parent / f"{project.name}-report"
    base = [ty, "check", "--python", ".venv", "--exit-zero", "--no-progress"]
    variants = {
        "check": base,
        "check --html-report": [*base, "--html-report", str(report_dir)],
    }

    # Warm the file system cache.
    measure(project, base)

    for name, cmd in variants.items():
        results = []
        for _ in range(runs):
            shutil.rmtree(report_dir, ignore_errors=True)
            result = measure(project, cmd)
            if result["code"] not in (0,):
                print(f"  {name}: exit code {result['code']}: {result['stderr']}")
            results.append(result)
        seconds = [r["seconds"] for r in results]
        rss = [r["rss_mb"] for r in results]
        print(
            f"{project.name:8} {name:22} "
            f"median {statistics.median(seconds):7.2f}s  (min {min(seconds):.2f}s, max {max(seconds):.2f}s)  "
            f"peak RSS {max(rss):7.0f} MB"
        )

    size = sum(f.stat().st_size for f in report_dir.rglob("*") if f.is_file())
    print(f"{project.name:8} report size {size / 1e6:.1f} MB, {sum(1 for _ in report_dir.rglob('*.html'))} pages")


if __name__ == "__main__":
    main()
