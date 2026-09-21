# Obsidian

Use Obsidian CLI for vault operations. Use DesktopCtl for other-app context and unsupported Obsidian UI.

If `command -v obsidian` fails, stop and tell the user:

> Open Obsidian Settings > General > Enable CLI

If the CLI exists but cannot connect to Obsidian, tell the user to open Obsidian fully and retry once. If it still fails, report the IPC failure and do not mutate the vault.

Optimize for one-shot execution:

* Infer intent and target from the prompt, active app, active note, and vault structure for read-only work.
* Do not ask follow-ups when a safe interpretation exists.
* Mutate vault content only when the request clearly implies a write.
* For writes, know the exact vault-relative target and bounded operation before acting.
* If write intent, target, and scope are clear, act without confirmation.
* Prefer the smallest semantically correct change.
* Prefer doing nothing over an unsafe or destructive guess.
* Verify every write.

Never infer an existing-note write target from the active note unless the user explicitly says “this note”, “current note”, or equivalent. For requests such as “add this to Obsidian”, default to creating a new note. Search for an existing destination only when the user names one, asks to update/merge/append, or clearly refers to an existing note.

## Escalation

1. Native `obsidian` CLI.
2. Read-only `app.*` via `obsidian eval`.
3. Narrow `app.*` mutation when necessary.
4. Narrow DOM action via `eval` as last resort.
5. DesktopCtl only for unsupported Obsidian UI.

Mutating `eval`, command execution, and DOM actions require a clearly requested outcome and a narrow understood target.

## Decision policy

Before writing, establish:

1. **Intent** — did the user clearly request a write?
2. **Target** — is there a sufficiently clear note/item/section to modify?
3. **Scope** — can the change be made narrowly and safely?

If all three are clear, act without asking.

If write intent is absent, remain read-only.

If target or scope is unsafe to infer, avoid mutation and return a short explanation.

Prefer the smallest semantically correct edit. Append only when the request and existing note structure support append.

Before mutating, establish the exact path, operation, and bounded content or edit range. Do not use a successful command exit status as permission to continue with a guessed repair.

## Active context

For “this/current note”, query live state:

```bash
obsidian eval code="app.workspace.getActiveFile()?.path"
```

Useful read-only queries:

```bash
# Parsed active-note metadata
obsidian eval code='JSON.stringify((()=>{const f=app.workspace.getActiveFile();return f?app.metadataCache.getFileCache(f):null})(),null,2)'

# Active frontmatter
obsidian eval code='JSON.stringify((()=>{const f=app.workspace.getActiveFile();return f?app.metadataCache.getFileCache(f)?.frontmatter??null:null})())'

# Vault notes
obsidian eval code="app.vault.getMarkdownFiles().map(f=>f.path)"

# Property vocabulary example
obsidian eval code="[...new Set(app.vault.getMarkdownFiles().map(f=>app.metadataCache.getFileCache(f)?.frontmatter?.type).filter(Boolean).flat())].sort()"
```

For links/backlinks, query or filter `app.metadataCache.resolvedLinks` for relevant notes. Do not dump the full graph unless explicitly requested.

Prefer small JSON-friendly queries and filter before returning large collections.

Use Obsidian APIs/`metadataCache` for links, backlinks, headings, tags, and frontmatter. Do not reconstruct Obsidian semantics with shell parsing.

Metadata cache may lag after writes; re-query before reporting success.

## Common recipes

### Capture into Obsidian

Examples: “save this”, “remember this”, “add this to my shopping list”, “put this in today’s note”.

Fast path for “add/save/capture this to Obsidian”:

1. Read relevant current-app context via DesktopCtl and copy/read the source once.
2. If the user names an existing note, use it. Otherwise create a new note by default.
3. Choose a concise title from the content and use title, URL, and folder context from the current-app snapshot; do not inspect unrelated vault notes.
4. Create with the known Obsidian CLI syntax:

   ```bash
   obsidian create path="..." content="..."
   ```

5. Read the created note once with `obsidian read` to verify.

For an explicitly named existing note or a request to update/merge/append, read the target, preserve its structure, make the smallest semantically correct change, and verify.

Default capture is a new note. Do not merge or append into an existing note unless the user requests it or names the target.

Ask a follow-up only when the title, target, or requested operation cannot be safely inferred. Do not silently choose the active note or daily note as an existing-note target.

For this fast path, do not run `obsidian help`, broad vault searches/listings, plugin discovery, read unrelated notes, or use the Web Clipper UI. Do not add unsupported flags such as `silent`. If `obsidian create` reports a path collision, stop and ask; do not search the vault to resolve it.

### Find / answer from Obsidian

Examples: “find my notes about X”, “what did I write about X?”, “what’s on my shopping list?”

1. Search using distinctive terms.
2. Read strongest matches.
3. Answer the user’s question, not just filenames.
4. Synthesize across notes when useful.

Prefer contextual search output when filenames alone are insufficient.

### Obsidian → another app

Examples: “reply using my notes”, “put my shopping list into this message”, “fill this using my saved details”.

1. Find/read relevant Obsidian content.
2. Extract only what the task needs.
3. Use DesktopCtl on the destination app.
4. Draft/fill rather than submit unless submission was clearly requested.
5. Verify destination content.

### Update existing content

Examples: “mark this done”, “add the price”, “update my project note”.

1. Find/read target.
2. Back it up in the agent session workspace.
3. Apply the smallest semantically correct change.
4. Preserve surrounding structure.
5. Re-read and verify.

If the backup cannot be created and verified, do not write.

### Create note from current context

Examples: “make a note from this”, “save this email/page as a note”.

1. Read source context via DesktopCtl.
2. Extract concise title/content.
3. Create a new note by default.
4. Only update an existing note if the user names it or asks to merge/update/append.
5. Follow nearby vault conventions; do not invent folders, tags, or metadata.
6. Verify.

## Workflows

### Find/open

```bash
obsidian search ...
obsidian open path="..."
```

Search, choose strongest match, open only when needed.

### Read

```bash
obsidian read path="..."
```

Use live active-note state when referring to the current note.

### Create

Create when the user’s request implies capture, saving, or a new note. New notes are the default when no existing target is named.

Search first only when the user asks to update, merge, append, or find an existing note. For default capture, avoid broad duplicate searches; check only the exact target path for a collision.

### Create then edit

For a new note that will receive substantial plain Markdown content:

1. Choose an exact vault-relative path and check for collisions.
2. Create it with Obsidian CLI using minimal initial content:

   ```bash
   obsidian create path="..." content="# ..."
   ```

3. Re-read it with `obsidian read` and confirm the created path.
4. Resolve the actual vault root; never assume the agent workspace or shell cwd is the vault.
5. For this newly-created note only, direct filesystem editing is allowed for plain Markdown. Write through a temporary file in the same directory, then atomically replace the note.
6. Re-read with `obsidian read` and re-query metadata before reporting success.

Do not use this shortcut for existing notes. Use Obsidian CLI/API for frontmatter, links, backlinks, plugin behavior, editor state, or other Obsidian semantics. If creation collides or verification is unexpected, stop without replacing anything.

### Append

Append only when both the request and note structure make append the semantically correct operation.

Preserve headings, frontmatter, checkbox style, list style, ordering, and formatting conventions.

### Targeted edit

Inspect first. Change only the smallest relevant section or item.

Avoid whole-note replacement when a narrower edit is possible.

For programmatic edits, require exactly one understood match before replacing text. Abort on zero or multiple matches. Never use broad “marker to end of file” cleanup or guessed regex repairs.

### Cross-app capture

DesktopCtl reads source context; Obsidian CLI performs vault mutation.

Do not foreground Obsidian unnecessarily.

Run mutation and verification as separate operations. If readback is unexpected, stop and report the anomaly; do not issue another mutation automatically.

## Backups

Before modifying an existing note, copy its pre-edit contents into the current agent session workspace:

```text
backup/<vault-relative-path>
```

Do not create `.bak` files inside the vault.

Do not back up newly created notes.

Backups do not justify risky edits.

## Safety

Act autonomously when:

* the user clearly requested a write
* the target is clear
* the mutation is narrow and understood

Typical safe writes:

* small append
* create a clearly requested note
* targeted task/property update
* narrow text edit

Safe does not mean implicit: the exact target and bounded change must still be known.

Use extra care and back up first for:

* paragraph/section replacement
* structured list/table edits
* several property changes
* rename/move

Require clear explicit intent for:

* delete
* whole-note overwrite
* bulk edit/rename/move
* broad vault reorganization
* arbitrary mutating `eval`
* arbitrary DOM mutation
* actions with unclear scope or side effects

Do not turn vague requests into destructive operations.

Do not attempt automatic cleanup after a failed, malformed, or surprising write. Report what changed and wait for explicit repair instructions.

## Eval

Treat `eval` according to what the JavaScript does.

Prefer:

* small expressions
* read-only inspection
* native Obsidian APIs
* structured output
* narrow scope

Avoid:

* giant inline programs
* large vault dumps
* full link-graph dumps
* DOM manipulation when an API exists
* broad mutations
* broad regex replacement or whole-note `app.vault.process()` edits
* undocumented internals when stable APIs exist

## Verification

After every mutation:

1. Re-read/re-query affected content.
2. Confirm the requested change exists.
3. Confirm unrelated surrounding content was preserved when relevant.

Do not report success only because a command succeeded. If verification is ambiguous or shows unexpected content, stop; do not repair or retry with another mutation.

## CLI usage

Use:

```bash
obsidian help <command>
```

when syntax, flags, defaults, or version-specific behavior are uncertain.

Prefer explicit scope and format. Keep `eval code=` and `content=` short. Avoid broad replacement, overwrite, delete, or DOM manipulation unless clearly required. Use the create-then-edit workflow above instead of passing large content or JavaScript payloads inline.

Do not chain a mutation with verification in one shell command. Avoid shell quoting tricks for multiline JavaScript or content; malformed escaping can turn help/output text into vault content.
