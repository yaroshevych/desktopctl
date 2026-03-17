#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'USAGE'
Usage:
  ask_claude.sh "your prompt"
  echo "your prompt" | ask_claude.sh
  ask_claude.sh --file /path/to/prompt.txt

Runs `claude -p` with a prompt passed via args, stdin, or file.
USAGE
}

if [[ "${1:-}" == "-h" || "${1:-}" == "--help" ]]; then
  usage
  exit 0
fi

prompt=""

if [[ "${1:-}" == "--file" ]]; then
  if [[ $# -lt 2 ]]; then
    echo "error: --file requires a path" >&2
    usage >&2
    exit 2
  fi
  file_path="$2"
  if [[ ! -f "$file_path" ]]; then
    echo "error: prompt file not found: $file_path" >&2
    exit 2
  fi
  prompt="$(cat "$file_path")"
elif [[ $# -gt 0 ]]; then
  prompt="$*"
elif [[ ! -t 0 ]]; then
  prompt="$(cat)"
fi

if [[ -z "${prompt//[[:space:]]/}" ]]; then
  echo "error: empty prompt" >&2
  usage >&2
  exit 2
fi

exec claude -p "$prompt"
