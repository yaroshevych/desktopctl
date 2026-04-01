# DesktopCtl

Computer Vision and mouse/keyboard control for local AI agents. Bring your own AI, observe UI, and perform complex operations with mouse, keyboard, and GPU-accelerated text recognition.

Stay in control with local runtime. Your AI agent should not take screenshots, and upload them to cloud.

Learn more at https://desktopctl.com

## Why DesktopCtl

- Local-first runtime, blazing fast with no cloud dependency in the core loop
- Bring your own AI: works with any desktop AI agent
- GPU-accelerated text recognition and computer vision
- Selector-first automation (`--text`, `--token`) with coordinate fallback
- Agent-friendly explicit waits and post-action verification
- Stable JSON contracts for agent integrations

## Architecture

DesktopCtl is split into two binaries:

- `DesktopCtl.app` (`desktopctld`): daemon that owns perception, state, execution, and verification
- `desktopctl`: stateless CLI surface for actions and queries over local IPC

Repository layout:

- `src/desktop/core` - shared protocol and types
- `src/desktop/daemon` - daemon runtime
- `src/desktop/cli` - CLI client

## Current Scope

- macOS-first
- OCR-first perception pipeline
- Tokenized screen output for agent grounding
- Deterministic CLI primitives for click/type/wait flows

## Prerequisites

- macOS (current support target)
- Rust toolchain (`cargo`)
- `just` command runner
- Accessibility permission for `DesktopCtl.app`
- Screen Recording permission for `DesktopCtl.app`

## Quick Start

```bash
just build run
```

```bash
raw="$(desktopctl app open Notes --json)"
win_id="$(printf '%s' "$raw" | jq -r '.result.window_id // empty')"
desktopctl keyboard press cmd+f --active-window "$win_id" --no-observe
desktopctl keyboard type "Shopping list" --active-window "$win_id" --no-observe
desktopctl screen tokenize --active-window "$win_id"
```

## Status / Roadmap

- Status: active development, with macOS-first CLI and daemon workflows already usable.
- Reliability for text/token-driven actions and verification loops. Stable machine-readable error codes.
- Upcoming CLI: `doctor`, richer `window/app` introspection, and `--explain` failure output.
- Better local computer vision and semantic UI tokenization.
- Multi-platform support.
