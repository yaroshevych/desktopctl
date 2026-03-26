# vm scripts

Host-driven scripts for managing a macOS VM used in development and testing.

VM connection is configured via `src/desktop/.env` (see `.env.example`):

```
VM_HOST=<ssh-host>
VM_USER=<vm-username>
VM_WINDOW_APP=UTM
```

## Scripts

- `enable_permissions.sh` — deploy DesktopCtl to the VM and grant Accessibility + Screen Recording permissions via host automation
- `smoke.sh` — run a smoke test suite against the VM (optionally runs permission flow first)
- `tokenize_phase0.sh` — environment lock: baseline screenshots and doctor checks before a tokenize run
- `tokenize_phase1_capture.sh` — capture screenshot corpus across apps and themes on the VM
- `test_notes_password_input.sh` — isolated test for character-by-character password input via Notes
