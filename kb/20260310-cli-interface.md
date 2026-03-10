# desktopctl CLI Interface (Brief)

Borrow from `kubectl`/`docker`/`gh`:

- Resource + verb: `desktopctl <resource> <verb>`
- Machine output: `-o json|yaml|table`, `--json`
- Declarative + imperative: `apply -f` and `batch`
- Inspectability: `get`, `describe`, `wait`
- Global flags: `--timeout`, `--dry-run`, `-v`

## Proposed Shape

- `desktopctl open <name> [-- <open-args...>]`
- `desktopctl open spotlight`
- `desktopctl open launchpad`
- `desktopctl pointer move <x> <y>`
- `desktopctl pointer down <x> <y>`
- `desktopctl pointer up <x> <y>`
- `desktopctl pointer click <x> <y>`
- `desktopctl pointer drag <x1> <y1> <x2> <y2> [hold_ms]`
- `desktopctl type "text"`
- `desktopctl key press <key-or-hotkey>`
- `desktopctl wait <ms>`
- `desktopctl get apps|windows|elements -o json`
- `desktopctl describe element <id>`
- `desktopctl batch -f flow.txt`
- `desktopctl apply -f flow.yaml`

## Chaining Commands

Three layers, in order of complexity:

**1. Shell `&&` — free, no special syntax**
```sh
desktopctl open Calculator && \
desktopctl wait 1000 && \
desktopctl pointer click 100 200
```
Each invocation is a separate process. Fast enough (Unix socket round-trip ~0.1ms). Fully composable with shell scripts.

**2. `batch` — one connection, no YAML**
```sh
desktopctl batch <<EOF
open Calculator
wait 1000
pointer click 100 200
type "123"
EOF
```
Or from a file: `desktopctl batch -f commands.txt`
Single daemon connection, stops on first error. No parsing ambiguity.

**3. `apply -f flow.yaml` — structured, reusable, shareable**
```yaml
name: open-calculator
steps:
  - open Calculator
  - wait 1000
  - pointer click 100 200
  - type "123"
```
The skills/playbooks layer. Supports naming, sharing via web service, eventually variables and conditions.

## Recommendation

Use a hybrid MVP:

1. Keep primitive resource/verb commands.
2. Shell `&&` chains work out of the box — no implementation needed.
3. Add `batch` for multi-step flows in a single connection.
4. Defer `apply -f` until the skills system is ready.
5. Make every command automation-friendly with structured output.

---

## Future Considerations

**1. Richer click targets (beyond raw coordinates)**
`pointer click <x> <y>` is too low-level as the primary interface — coordinates are brittle across resolutions and window positions. Add a target selector syntax alongside raw coords:
```
desktopctl pointer click --text "Send"
desktopctl pointer click --id button_123
desktopctl pointer click 100 200     # fallback: raw coords
```

**2. Screen capture command**
Agents need to observe UI state. Add `desktopctl screen capture` (full screen or region) as the fallback when accessibility APIs return nothing. Pairs with OCR/vision for element detection.

**3. `apply -f` for reusable playbooks**
For complex, named, shareable flows — YAML pipelines with variables and conditions. Part of the skills system.

**4. Daemon management commands**
Since a long-running daemon holds OS permissions, add lifecycle commands:
```
desktopctl daemon status
desktopctl daemon start
desktopctl daemon stop
```

**5. `open <App> <file>` — open a file with a specific app**
```sh
desktopctl open Preview "/path/to/image.jpg" --wait
```
Avoids mixing native `open -a` with `desktopctl` in the same workflow.

**6. Launchpad is not a regular app**
`desktopctl open Launchpad` should be treated as a special case — Launchpad is a system UI layer, not an app. Prefer `desktopctl open Calculator` directly (resolves via OS app lookup) over routing through Launchpad.
