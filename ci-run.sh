#!/usr/bin/env bash
# Runs the coverage comparison with the environment ty's ecosystem CI job uses.
set -euo pipefail
S=/tmp/claude-0/-home-user-ruff/e0cf2ee9-f011-5f9b-96bb-0ffd436770ca/scratchpad
export PATH="$HOME/.local/bin:$PATH"
export TY_UV=scripts
export UV_CONSTRAINT=/home/user/ruff/.github/ty-ecosystem-constraints.txt
export XDG_CONFIG_HOME=$S/ci-run/config
export XDG_CACHE_HOME=$S/ci-run/cache
cd $S/primer-src/ea
exec .venv/bin/ecosystem-analyzer coverage-compare \
  --ty-binary "${TY_BINARY:-$S/ci-run/ty}" \
  --mypy $S/primer/mypy-venv/bin/mypy \
  --exclude-newer 2026-11-12T00:00:00Z \
  --output "${OUTPUT:-$S/ci-run/results.jsonl}" \
  --work-dir $S/ci-run/work "$@"
