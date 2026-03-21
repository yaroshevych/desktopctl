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

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
env_file="${DESKTOPCTL_ENV_FILE:-$script_dir/../../.env}"
if [[ -f "$env_file" ]]; then
  set -a
  # shellcheck disable=SC1090
  source "$env_file"
  set +a
fi

project_root="${DESKTOPCTL_PROJECT_ROOT:-}"
if [[ -z "$project_root" ]]; then
  project_root="$(git -C "$script_dir" rev-parse --show-toplevel 2>/dev/null || true)"
fi
if [[ -z "$project_root" ]]; then
  echo "error: set DESKTOPCTL_PROJECT_ROOT in $env_file or run from a git checkout" >&2
  exit 2
fi

log_file="${DESKTOPCTL_ASK_CLAUDE_LOG:-$project_root/tmp/ask_claude.md}"
mkdir -p "$(dirname "$log_file")"

answer="$(cd "$project_root" && claude -p "$prompt")"

{
  echo "## $(date '+%Y-%m-%d %H:%M:%S')"
  echo ""
  echo "**Q:** $prompt"
  echo ""
  echo "**A:** $answer"
  echo ""
  echo "---"
  echo ""
} >> "$log_file"

printf '%s\n' "$answer"
