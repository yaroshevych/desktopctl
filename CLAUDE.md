# Project Instructions for AI Agents

This file provides instructions and context for AI coding agents working on this project.

<!-- BEGIN BEADS INTEGRATION v:1 profile:minimal hash:ca08a54f -->
## Beads Issue Tracker

This project uses **bd (beads)** for issue tracking. Run `bd prime` to see full workflow context and commands.

### Quick Reference

```bash
bd ready              # Find available work
bd show <id>          # View issue details
bd update <id> --claim  # Claim work
bd close <id>         # Complete work
```

### Rules

- Use `bd` for ALL task tracking — do NOT use TodoWrite, TaskCreate, or markdown TODO lists
- Run `bd prime` for detailed command reference and session close protocol
- Use `bd remember` for persistent knowledge — do NOT use MEMORY.md files

## Session Completion

**When ending a work session**, complete ALL steps below. **Do NOT push** - that is done by human.

**MANDATORY WORKFLOW:**

1. **File issues for remaining work** - Create issues for anything that needs follow-up
2. **Run quality gates** (if code changed) - Tests, linters, builds
3. **Update issue status** - Close finished work, update in-progress items
4. **Notify human** - Inform human that work is ready for push
5. **Clean up** - Clear stashes, prune remote branches
6. **Verify** - All changes committed locally
7. **Hand off** - Provide context for next session

**CRITICAL RULES:**
- NEVER push or pull - remote operations are human-only
- NEVER run `git push`, `git pull`, `bd dolt push/pull`
<!-- END BEADS INTEGRATION -->

## Remote Operations

All remote operations (push, pull, fetch, dolt push/pull) are **DONE BY HUMAN ONLY**.

## Commits

When making commits, keep messages brief and descriptive:
- Use imperative mood ("Add", "Fix", "Update", not "Added", "Fixed")
- Be concise: "Windows permissions dialog" not "This commit adds a dialog..."
- Include scope when helpful: "tray: embedded idle+active icons"

## Build & Test

_Add your build and test commands here_

```bash
# Example:
# npm install
# npm test
```

## Architecture Overview

_Add a brief overview of your project architecture_

## Conventions & Patterns

### Tools
- Use `rg` instead of `grep`
- Use `fd` instead of `find`

### Known Issues
- Test `golden_controls_have_expected_text_fields_and_buttons` is currently failing - **do NOT attempt to fix this test**
