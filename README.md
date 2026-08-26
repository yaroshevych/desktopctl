# DesktopCtl

Local CLI for AI agents to observe and control your computer via screen, mouse, and keyboard. Bring your own AI - any model, even without vision.

Runs fully local. No screenshots sent to the cloud.

Learn more at https://desktopctl.com

https://github.com/user-attachments/assets/4321b23e-6706-4792-a911-89e13766ebc0

## Why DesktopCtl

- Local-first runtime. No cloud dependency
- Bring your own AI: works with any desktop AI agent
- GPU-accelerated text recognition and computer vision
- Selector-first automation (`--text`, `--token`) with coordinate fallback
- Agent-friendly explicit waits and post-action verification
- Stable JSON contracts for agent integrations

## Architecture

DesktopCtl is split into two binaries:

- `DesktopCtl.app` (`desktopctld`): daemon that owns perception, state, execution, and verification
- `desktopctl`: stateless CLI surface for actions and queries over local IPC

## Filesystem layout

DesktopCtl uses the same Linux-style application directory on every platform:

```text
${DESKTOPCTL_HOME:-${XDG_DATA_HOME:-$HOME/.local/share}/desktopctl}/
├── config.toml
├── state/
├── logs/
├── cache/
└── workspaces/
```

Path precedence is `DESKTOPCTL_HOME`, then `XDG_DATA_HOME/desktopctl`, then
`$HOME/.local/share/desktopctl`. Directories are created only when their
contents are needed. Configuration lives in `config.toml`, persistent runtime
metadata in `state/`, launcher sessions in `workspaces/`, logs and journal
output in `logs/`, and disposable screenshots/OCR/debug output in `cache/`.
Unix runtime sockets remain in the private temporary runtime directory.

Exact-path overrides remain available for specialized use:
`DESKTOPCTL_SOCKET_PATH`, `DESKTOPCTL_IPC_TOKEN_PATH`, `DESKTOPCTL_TRACE_PATH`,
`DESKTOPCTL_CLI_TRACE_PATH`, and `DESKTOPCTL_RECORD_BASE`. These override only
their named artifact; `DESKTOPCTL_HOME` controls the application root.

On first startup, DesktopCtl safely copies valid legacy JSON configuration and
launcher sessions from `$XDG_CONFIG_HOME/desktopctl`,
`$HOME/.config/desktopctl`, `$HOME/Library/Application Support/DesktopCtl`, and
the former XDG data root into the new layout. Existing destinations are never
overwritten, conflicting legacy sources are left untouched, and migration
errors are reported without deleting old files. `XDG_CONFIG_HOME` participates
only in legacy migration; it does not control the new layout.

Repository layout:

- `src/desktop/core` - shared protocol and types
- `src/desktop/daemon` - daemon runtime
- `src/desktop/cli` - CLI client

## Current Scope

- macOS-first
- OCR-first perception pipeline
- Tokenized screen output for agent grounding
- Deterministic CLI primitives for click/type/wait flows
- Native macOS menu enumeration and invocation through Accessibility

## Prerequisites

- macOS (current support target)
- Rust toolchain (`cargo`)
- `just` command runner
- Accessibility permission for `DesktopCtl.app`
- Screen Recording permission for `DesktopCtl.app`

## Quick Start

```bash
make install
```

```bash
raw="$(desktopctl app open Notes --json)"
win_id="$(printf '%s' "$raw" | jq -r '.result.window_id // empty')"
desktopctl keyboard press cmd+f --active-window "$win_id" --no-observe
desktopctl keyboard type "Shopping list" --active-window "$win_id" --no-observe
desktopctl screen tokenize --active-window "$win_id"
```

List native menus, then invoke an item by its returned `#menu_*` ID:

```bash
desktopctl menu list --active-window "$win_id"
desktopctl menu click --id menu_file_new_window --active-window "$win_id"
```

Menu listing omits the Apple/system menu, separators, and long-list overflow by default. Use `--system` to include the Apple menu and `--all` to show every item.

## Status / Roadmap

- Status: active development, with macOS-first CLI and daemon workflows already usable.
- Reliability for text/token-driven actions and verification loops. Stable machine-readable error codes.
- Upcoming CLI: `doctor`, richer `window/app` introspection, and `--explain` failure output.
- Better local computer vision and semantic UI tokenization.
- Multi-platform support.
