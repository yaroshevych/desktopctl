# Project Instructions for AI Agents

This file provides instructions and context for AI coding agents working on this project.

## Remote Operations

**IMPORTANT**: All remote operations (push, pull, fetch) are **DONE BY HUMAN ONLY**.

## Commits

When making commits, keep messages brief and descriptive:
- Use imperative mood ("Add", "Fix", "Update", not "Added", "Fixed")
- Be concise: "Windows permissions dialog" not "This commit adds a dialog..."
- Include scope when helpful: "tray: embedded idle+active icons"

## Build & Test

Primary desktop commands live in `src/desktop/Justfile`.

```bash
# Build the macOS app and CLI artifacts
just -f src/desktop/Justfile build

# Compile CLI and daemon tests without running them
just -f src/desktop/Justfile test-compile

# Run release gates, excluding the stricter clippy pass
just -f src/desktop/Justfile release-gates

# Optional strict gate
just -f src/desktop/Justfile release-gates-strict
```

Note: `release-gates` currently runs the known failing
`golden_controls_have_expected_text_fields_and_buttons` test. Do not fix that
test unless explicitly asked.

## Architecture Overview

_Add a brief overview of your project architecture_

## Conventions & Patterns

### Tools
- Use `rg` instead of `grep`
- Use `fd` instead of `find`

### Known Issues
- Test `golden_controls_have_expected_text_fields_and_buttons` is currently failing - **do NOT attempt to fix this test**
