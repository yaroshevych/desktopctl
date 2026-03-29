#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/../../../.." && pwd)"
USAGE_SRC="${REPO_ROOT}/src/desktop/cli/src/usage.rs"
CLI_MD="${REPO_ROOT}/CLI.md"

if [[ ! -f "${USAGE_SRC}" ]]; then
  echo "error: missing usage source: ${USAGE_SRC}" >&2
  exit 1
fi
if [[ ! -f "${CLI_MD}" ]]; then
  echo "error: missing CLI reference doc: ${CLI_MD}" >&2
  exit 1
fi

tmpdir="$(mktemp -d)"
trap 'rm -rf "${tmpdir}"' EXIT

normalize_lines() {
  perl -pe '
    s/\\"/"/g;
    s/\[[^\]]*\]//g;
    s/[\[\]]//g;
    s/\(--title <text> \| --id <id>\)/--title <text>/g;
    s/ --json//g;
    s/ --absolute//g;
    s/ --duration <ms>//g;
    s/(desktopctl window (?:bounds|focus)) --id <id>/$1 --title <text>/g;
    s/(<[^>]+>)"$/$1/;
    s/[[:blank:]]+/ /g;
    s/^[[:blank:]]+|[[:blank:]]+$//g;
  ' | sed '/^$/d' | sort -u
}

extract_usage_commands() {
  awk '/usage\(\)/,/^}/' "${USAGE_SRC}" \
    | sed -n 's/^[[:space:]]*desktopctl /desktopctl /p'
}

extract_cli_md_commands() {
  grep '^desktopctl ' "${CLI_MD}" | grep -v 'desktopctl <command...>'
}

extract_usage_commands | normalize_lines > "${tmpdir}/usage.txt"
extract_cli_md_commands | normalize_lines > "${tmpdir}/cli_md.txt"

missing_in_doc="$(comm -23 "${tmpdir}/usage.txt" "${tmpdir}/cli_md.txt" || true)"
missing_in_usage="$(comm -13 "${tmpdir}/usage.txt" "${tmpdir}/cli_md.txt" || true)"

if [[ -n "${missing_in_doc}" || -n "${missing_in_usage}" ]]; then
  echo "CLI reference mismatch detected between usage() and CLI.md" >&2
  if [[ -n "${missing_in_doc}" ]]; then
    echo "" >&2
    echo "present in usage(), missing in CLI.md:" >&2
    echo "${missing_in_doc}" >&2
  fi
  if [[ -n "${missing_in_usage}" ]]; then
    echo "" >&2
    echo "present in CLI.md, missing in usage():" >&2
    echo "${missing_in_usage}" >&2
  fi
  exit 1
fi

echo "cli-reference-check: OK"
