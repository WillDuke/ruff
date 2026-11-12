# /// script
# requires-python = ">=3.11"
# dependencies = []
# ///
"""Compare ty's `--html-report` with mypy's on the mypy_primer projects.

Usage: run_primer.py PROJECTS_PY TY MYPY WORK_DIR RESULTS_JSONL

For each project (cheapest first), this clones it, creates a venv with its dependencies, and then
measures:

- `ty check` without and with `--html-report` (wall time and peak RSS),
- mypy with `--html-report` (wall time and peak RSS),

using the project's own mypy_primer commands. It then compares the two reports line by line on the
files that appear in both, and appends one JSON object per project to RESULTS_JSONL. Projects that
already have a result are skipped, so the script can be restarted. The project and its reports are
deleted afterwards to save disk space.
"""

import ast
import html
import json
import os
import re
import shlex
import shutil
import subprocess
import sys
import time
from collections import Counter
from pathlib import Path

CLONE_TIMEOUT = 600
INSTALL_TIMEOUT = 900
TY_TIMEOUT = 900
MYPY_TIMEOUT = 1800

MEASURE = """
import json, resource, subprocess, sys, time
start = time.perf_counter()
try:
    proc = subprocess.run(sys.argv[3], shell=True, cwd=sys.argv[1], stdout=subprocess.PIPE,
                          stderr=subprocess.STDOUT, text=True, errors="replace",
                          timeout=float(sys.argv[2]))
    code, out = proc.returncode, proc.stdout
except subprocess.TimeoutExpired as e:
    code, out = "timeout", ""
elapsed = time.perf_counter() - start
rss_kb = resource.getrusage(resource.RUSAGE_CHILDREN).ru_maxrss
print(json.dumps({"seconds": elapsed, "rss_mb": rss_kb / 1024, "code": code, "tail": out[-800:]}))
"""


def load_projects(projects_py: Path) -> list[dict]:
    tree = ast.parse(projects_py.read_text())
    projects = []
    for node in ast.walk(tree):
        if isinstance(node, ast.Call) and getattr(node.func, "id", None) == "Project":
            projects.append({kw.arg: ast.literal_eval(kw.value) for kw in node.keywords})
    return sorted(projects, key=lambda p: p.get("cost", {}).get("mypy", 0))


def project_name(project: dict) -> str:
    return project.get("name_override") or project["location"].rstrip("/").split("/")[-1]


def measure(cwd: Path, cmd: str, timeout: int) -> dict:
    out = subprocess.run(
        [sys.executable, "-c", MEASURE, str(cwd), str(timeout), cmd],
        capture_output=True,
        text=True,
        check=True,
    )
    return json.loads(out.stdout)


def run(cmd: str, cwd: Path, timeout: int) -> None:
    subprocess.run(cmd, shell=True, cwd=cwd, check=True, timeout=timeout, capture_output=True)


def format_cmd(template: str, **values: str) -> str:
    for key, value in values.items():
        template = template.replace("{" + key + "}", value)
    return template


def setup(project: dict, repo: Path, uv: str) -> None:
    run(f"git clone -q --depth 1 {shlex.quote(project['location'])} {shlex.quote(str(repo))}",
        repo.parent, CLONE_TIMEOUT)
    run(f"{uv} venv -q .venv --python 3.12 --seed", repo, INSTALL_TIMEOUT)
    install = f"{uv} pip install -q --python .venv/bin/python"
    if project.get("install_cmd"):
        run(format_cmd(project["install_cmd"], install=install), repo, INSTALL_TIMEOUT)
    if project.get("deps"):
        run(f"{install} {' '.join(shlex.quote(d) for d in project['deps'])}", repo, INSTALL_TIMEOUT)


TY_LINE = re.compile(r'<div class="line (\w+)" id="L\d+"(?: title="([^"]*)")?')
MYPY_LINE = re.compile(r'<span class="line-(\w+)" title="([^"]*)"')
FLAGGED = {"imprecise", "dynamic", "any"}
CATEGORY = {"empty": "empty", "unanalyzed": "empty", "precise": "precise",
            "imprecise": "imprecise", "any": "dynamic", "dynamic": "dynamic"}


def report_pages(root: Path, subdir: str) -> dict[str, Path]:
    base = root / subdir
    if not base.is_dir():
        return {}
    return {str(p.relative_to(base))[: -len(".html")]: p for p in base.rglob("*.html")}


def compare(ty_report: Path, mypy_report: Path) -> dict:
    ty_pages = report_pages(ty_report, "files")
    mypy_pages = report_pages(mypy_report, "html")

    def counts(lines: list[str]) -> dict:
        c = {"lines": len(lines), "flagged": sum(1 for l in lines if l in FLAGGED)}
        return c

    ty_total = {"lines": 0, "flagged": 0}
    mypy_total = {"lines": 0, "flagged": 0}
    matched = {"files": 0, "lines": 0, "ty_flagged": 0, "mypy_flagged": 0, "agree": 0,
               "agree_exact": 0, "ty_only": 0, "mypy_only": 0, "line_count_mismatch": 0}

    # Why the reports disagree: for lines only ty flags, the type in ty's first note; for lines
    # only mypy flags, the kinds of `Any` that mypy lists.
    ty_only_reasons: Counter = Counter()
    mypy_only_reasons: Counter = Counter()
    examples: dict = {}

    mypy_lines = {}
    mypy_titles = {}
    for rel, path in mypy_pages.items():
        found = MYPY_LINE.findall(path.read_text(errors="replace"))
        lines = [c for c, _ in found]
        mypy_lines[rel] = lines
        mypy_titles[rel] = [html.unescape(t) for _, t in found]
        for k, v in counts(lines).items():
            mypy_total[k] += v

    for rel, path in ty_pages.items():
        found = TY_LINE.findall(path.read_text(errors="replace"))
        t = [c for c, _ in found]
        notes = [html.unescape(n) for _, n in found]
        for k, v in counts(t).items():
            ty_total[k] += v
        candidates = [m for m in mypy_lines if m == rel or m.endswith("/" + rel)]
        if len(candidates) != 1:
            continue
        m = mypy_lines[candidates[0]]
        titles = mypy_titles[candidates[0]]
        if len(m) != len(t):
            matched["line_count_mismatch"] += 1
        matched["files"] += 1
        for number, (tc, mc) in enumerate(zip(t, m)):
            matched["lines"] += 1
            tf, mf = tc in FLAGGED, mc in FLAGGED
            matched["ty_flagged"] += tf
            matched["mypy_flagged"] += mf
            matched["agree"] += tf == mf
            matched["agree_exact"] += CATEGORY.get(tc) == CATEGORY.get(mc)
            matched["ty_only"] += tf and not mf
            matched["mypy_only"] += mf and not tf
            if tf and not mf:
                reason = re.sub(r"Literal\[[^\]]*\]", "Literal[…]",
                                notes[number].split("\n")[0].split("`: ", 1)[-1])[:60]
                ty_only_reasons[reason] += 1
                examples.setdefault("ty: " + reason, f"{rel}:{number + 1}")
            elif mf and not tf:
                kinds = re.findall(r"\n(.+?) \(x\d+\)", titles[number]) or [mc]
                reason = ", ".join(sorted(set(kinds)))
                mypy_only_reasons[reason] += 1
                examples.setdefault("mypy: " + reason, f"{rel}:{number + 1}")

    return {
        "ty_files": len(ty_pages), "mypy_files": len(mypy_pages),
        "ty_total": ty_total, "mypy_total": mypy_total, "matched": matched,
        "ty_only_reasons": ty_only_reasons.most_common(8),
        "mypy_only_reasons": mypy_only_reasons.most_common(8),
        "examples": examples,
    }


def main() -> None:
    projects_py, ty, mypy, work, results = sys.argv[1:6]
    work = Path(work).resolve()
    results = Path(results).resolve()
    uv = shutil.which("uv") or os.path.expanduser("~/.local/bin/uv")
    work.mkdir(parents=True, exist_ok=True)

    done = set()
    if results.exists():
        done = {json.loads(line)["name"] for line in results.read_text().splitlines() if line}

    limit = int(os.environ.get("PRIMER_LIMIT", "0")) or None
    for project in load_projects(Path(projects_py))[:limit]:
        name = project_name(project)
        if name in done:
            continue
        # Keep the uv cache from filling the disk.
        if shutil.disk_usage(work).free < 6 * 1024**3:
            subprocess.run([uv, "cache", "clean"], capture_output=True, check=False)
        repo = work / name
        ty_report = work / f"{name}-ty-report"
        mypy_report = work / f"{name}-mypy-report"
        for path in (repo, ty_report, mypy_report):
            shutil.rmtree(path, ignore_errors=True)

        result: dict = {"name": name, "location": project["location"],
                        "mypy_cost": project.get("cost", {}).get("mypy"),
                        "needs_mypy_plugins": bool(project.get("needs_mypy_plugins"))}
        started = time.time()
        try:
            setup(project, repo, uv)
        except Exception as e:  # noqa: BLE001
            stderr = getattr(e, "stderr", b"") or b""
            result["error"] = f"setup failed: {type(e).__name__}: {stderr[-500:]!r}"
        else:
            paths = " ".join(project.get("paths") or [])
            ty_cmd = project.get("ty_cmd") or ("{ty} check {paths}" if paths else "{ty} check")
            ty_cmd = format_cmd(ty_cmd, ty=ty, paths=paths)
            ty_cmd += " --python .venv --output-format concise --no-progress"
            result["ty_check"] = measure(repo, ty_cmd, TY_TIMEOUT)
            result["ty_report"] = measure(
                repo, f"{ty_cmd} --html-report {shlex.quote(str(ty_report))}", TY_TIMEOUT
            )

            mypy_cmd = project.get("mypy_cmd") or ("{mypy} {paths}" if paths else "{mypy} .")
            mypy_cmd = format_cmd(mypy_cmd, mypy=mypy, paths=paths)
            mypy_cmd += (" --python-executable=.venv/bin/python --soft-error-limit=-1"
                         f" --html-report {shlex.quote(str(mypy_report))}")
            no_cache = f" --no-incremental --cache-dir={os.devnull}"
            result["mypy_report"] = measure(repo, mypy_cmd + no_cache, MYPY_TIMEOUT)
            if "Cache must be enabled in parallel mode" in result["mypy_report"]["tail"]:
                # The project's configuration enables mypy's parallel mode, which needs a cache.
                cache = f" --cache-dir={shlex.quote(str(work / (name + '-mypy-cache')))}"
                result["mypy_report"] = measure(repo, mypy_cmd + cache, MYPY_TIMEOUT)
                result["mypy_parallel_mode"] = True

            try:
                result["comparison"] = compare(ty_report, mypy_report)
            except Exception as e:  # noqa: BLE001
                result["comparison_error"] = f"{type(e).__name__}: {e}"

        result["total_seconds"] = time.time() - started
        with results.open("a") as f:
            f.write(json.dumps(result) + "\n")
        print(f"{name}: {json.dumps(result.get('comparison', {}).get('matched', result.get('error')))}",
              flush=True)

        for path in (repo, ty_report, mypy_report, work / f"{name}-mypy-cache"):
            shutil.rmtree(path, ignore_errors=True)


if __name__ == "__main__":
    main()
