#!/usr/bin/env bash
set -euo pipefail

REPO="yaroshevych/desktopctl"
API_URL="https://api.github.com/repos/${REPO}/releases/latest"
ASSET_NAME="desktopctl-darwin-arm64.tar.gz"
BIN_DIR="${HOME}/.local/bin"
BIN_PATH="${BIN_DIR}/desktopctl"
APP_DIR="${HOME}/Applications"
APP_PATH="${APP_DIR}/DesktopCtl.app"

die() {
  printf 'desktopctl install: %s\n' "$*" >&2
  exit 1
}

info() {
  printf 'desktopctl install: %s\n' "$*"
}

[[ "$(uname -s)" == "Darwin" ]] || die "macOS is required."
[[ "$(uname -m)" == "arm64" ]] || die "Apple Silicon (arm64) is required; Intel Macs are not supported."

for command_name in curl tar mktemp awk shasum xattr open osascript find tr; do
  command -v "$command_name" >/dev/null 2>&1 || die "required command not found: ${command_name}"
done

tmp_dir="$(mktemp -d)"
cli_stage=""
cli_backup=""
cli_install_started=false
app_stage=""
app_backup=""
app_install_started=false
install_committed=false

cleanup() {
  local status=$?

  if [[ "$install_committed" != true ]]; then
    if [[ "$app_install_started" == true && -e "$APP_PATH" ]]; then
      rm -rf "$APP_PATH" || true
    fi
    if [[ "$cli_install_started" == true && -e "$BIN_PATH" ]]; then
      rm -f "$BIN_PATH" || true
    fi
    if [[ -n "$app_backup" && -e "$app_backup" ]]; then
      if mv "$app_backup" "$APP_PATH"; then
        app_backup=""
      else
        printf 'desktopctl install: previous app preserved at %s\n' "$app_backup" >&2
        app_backup=""
      fi
    fi
    if [[ -n "$cli_backup" && -e "$cli_backup" ]]; then
      if mv "$cli_backup" "$BIN_PATH"; then
        cli_backup=""
      else
        printf 'desktopctl install: previous CLI preserved at %s\n' "$cli_backup" >&2
        cli_backup=""
      fi
    fi
  fi

  rm -rf "$tmp_dir" || true
  [[ -z "$cli_stage" ]] || rm -f "$cli_stage" || true
  [[ -z "$app_stage" ]] || rm -rf "$app_stage" || true
  [[ -z "$app_backup" ]] || rm -rf "$app_backup" || true
  [[ -z "$cli_backup" ]] || rm -f "$cli_backup" || true
  return "$status"
}
trap cleanup EXIT
trap 'exit 129' HUP
trap 'exit 130' INT
trap 'exit 143' TERM

info "resolving latest release"
release_json="$(curl -fsSL --connect-timeout 15 --max-time 300 --retry 3 --retry-delay 1 "$API_URL")" ||
  die "could not query the latest GitHub release"

# macOS ships JavaScript for Automation, which parses JSON without jq and
# without depending on whitespace or object ordering in the API response.
release_info="$(printf '%s' "$release_json" | osascript -l JavaScript -e '
ObjC.import("Foundation");
const data = JSON.parse(ObjC.unwrap($.NSString.alloc.initWithDataEncoding(
  $.NSFileHandle.fileHandleWithStandardInput.readDataToEndOfFile,
  $.NSUTF8StringEncoding
)));
function writeLine(value) {
  const data = $.NSString.alloc.initWithString(value + "\\n").dataUsingEncoding($.NSUTF8StringEncoding);
  $.NSFileHandle.fileHandleWithStandardOutput.writeData(data);
}
let asset = null;
for (const candidate of (data.assets || [])) {
  if (candidate.name === "desktopctl-darwin-arm64.tar.gz") {
    asset = candidate;
    break;
  }
}
writeLine(data.tag_name || "");
writeLine(asset ? (asset.browser_download_url || "") : "");
writeLine(asset ? (asset.digest || "") : "");
')" || die "could not parse the latest GitHub release"

release_tag="$(printf '%s\n' "$release_info" | awk 'NR == 1 { print; exit }')"
download_url="$(printf '%s\n' "$release_info" | awk 'NR == 2 { print; exit }')"
asset_digest="$(printf '%s\n' "$release_info" | awk 'NR == 3 { print; exit }')"
[[ -n "$release_tag" ]] || die "latest GitHub release did not contain tag_name"
[[ "$download_url" == "https://github.com/${REPO}/releases/download/"*"/${ASSET_NAME}" ]] ||
  die "latest release does not contain ${ASSET_NAME}"

case "$asset_digest" in
  "") expected_sha="" ;;
  sha256:*) expected_sha="$(printf '%s' "${asset_digest#sha256:}" | tr '[:upper:]' '[:lower:]')" ;;
  *) die "release API returned an invalid digest for ${ASSET_NAME}" ;;
esac
if [[ -n "$expected_sha" && ! "$expected_sha" =~ ^[0-9A-Fa-f]{64}$ ]]; then
  die "release API returned an invalid SHA-256 digest for ${ASSET_NAME}"
fi

archive_path="${tmp_dir}/${ASSET_NAME}"
info "downloading ${ASSET_NAME} (${release_tag})"
curl -fL --connect-timeout 15 --max-time 300 --retry 3 --retry-delay 1 -o "$archive_path" "$download_url" ||
  die "download failed: ${download_url}"

if [[ -n "$expected_sha" ]]; then
  actual_sha="$(shasum -a 256 "$archive_path" | awk '{print $1}')"
  [[ "$actual_sha" == "$expected_sha" ]] ||
    die "SHA-256 mismatch for ${ASSET_NAME}"
  info "SHA-256 verified"
else
  # TODO: publish a checksums.txt asset if GitHub API digests are unavailable.
  info "no SHA-256 digest published for ${ASSET_NAME}; skipping verification"
fi

extract_dir="${tmp_dir}/extract"
mkdir -p "$extract_dir"
members_path="${tmp_dir}/members"
tar -tzf "$archive_path" >"$members_path" || die "downloaded file is not a valid gzip archive"

if ! awk '
  BEGIN { app = cli = bad = 0 }
  {
    if (++seen[$0] > 1) bad = 1
    if ($0 == "DesktopCtl.app/") app++
    if ($0 == "desktopctl") cli++
    if ($0 ~ /^\// || $0 ~ /(^|\/)\.\.(\/|$)/ || $0 ~ /(^|\/)\.\/(\/|$)/) bad = 1
    split($0, parts, "/")
    if (parts[1] != "DesktopCtl.app" && $0 != "desktopctl") bad = 1
  }
  END { exit !(app == 1 && cli == 1 && !bad) }
' "$members_path"; then
  die "archive contains unexpected or unsafe paths"
fi

if ! tar -tvzf "$archive_path" | awk '$1 !~ /^[-d]/ { bad = 1 } END { exit bad }'; then
  die "archive contains non-file or non-directory members"
fi

tar -xzf "$archive_path" -C "$extract_dir" || die "could not extract ${ASSET_NAME}"

source_app="${extract_dir}/DesktopCtl.app"
source_cli="${extract_dir}/desktopctl"
[[ -d "$source_app" && ! -L "$source_app" ]] || die "archive did not contain DesktopCtl.app"
[[ -f "$source_cli" && ! -L "$source_cli" ]] || die "archive did not contain a regular desktopctl file"

mkdir -p "$BIN_DIR" "$APP_DIR"

cli_stage="$(mktemp "${BIN_DIR}/.desktopctl.XXXXXX")"
cp "$source_cli" "$cli_stage"
chmod 0755 "$cli_stage"

remove_quarantine_from_file() {
  local path="$1"
  local probe
  if probe="$(xattr -p com.apple.quarantine "$path" 2>&1)"; then
    xattr -d com.apple.quarantine "$path" >/dev/null 2>&1 ||
      die "could not remove quarantine from ${path}"
  elif [[ "$probe" != *"No such xattr"* ]]; then
    die "could not inspect quarantine on ${path}: ${probe}"
  fi
}

remove_quarantine_from_file "$cli_stage"

# Stage the bundle under ~/Applications so the final rename stays on the same
# filesystem. Both artifacts are staged before either live path is changed.
app_stage="$(mktemp -d "${APP_DIR}/.DesktopCtl.app.XXXXXX")"
cp -R "$source_app" "${app_stage}/DesktopCtl.app"
if ! xattr -dr com.apple.quarantine "${app_stage}/DesktopCtl.app" >/dev/null 2>&1; then
  while IFS= read -r -d '' member; do
    remove_quarantine_from_file "$member"
  done < <(find "${app_stage}/DesktopCtl.app" -print0)
fi

app_backup="$(mktemp -d "${APP_DIR}/.DesktopCtl.app.backup.XXXXXX")"
rmdir "$app_backup"
cli_backup="$(mktemp "${BIN_DIR}/.desktopctl.backup.XXXXXX")"
rm -f "$cli_backup"

if [[ -e "$APP_PATH" ]]; then
  mv "$APP_PATH" "$app_backup"
fi
if [[ -e "$BIN_PATH" ]]; then
  mv "$BIN_PATH" "$cli_backup"
fi

cli_install_started=true
mv -f "$cli_stage" "$BIN_PATH"
cli_stage=""

app_install_started=true
mv "${app_stage}/DesktopCtl.app" "$APP_PATH"
install_committed=true

rm -rf "$app_backup"
app_backup=""
rm -f "$cli_backup"
cli_backup=""
rmdir "$app_stage"
app_stage=""

if ! open "$APP_PATH"; then
  die "installed ${release_tag}, but could not launch ${APP_PATH}"
fi

info "installed ${release_tag}"
printf '  CLI: %s\n' "$BIN_PATH"
printf '  App: %s\n' "$APP_PATH"

case ":${PATH:-}:" in
  *":${BIN_DIR}:"*) ;;
  *) printf '  Add to PATH: export PATH="\$HOME/.local/bin:\$PATH"\n' ;;
esac
