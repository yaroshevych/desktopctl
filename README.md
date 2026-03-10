# DesktopCtl

`desktopctl` lets agents and developers control desktop applications through a deterministic CLI interface.

The desktop automation split into two binaries:

- `DesktopCtl.app` (`desktopctld`): long-running menubar daemon (owns permissions, executes actions)
- `desktopctl`: CLI client (sends commands to daemon over local IPC)

## Structure

- `src/desktop/core` — shared protocol, IPC, automation backend
- `src/desktop/daemon` — `DesktopCtl.app`
- `src/desktop/cli` — `desktopctl`

## Build / Run

```bash
just build
just run
```

## Examples

```bash
desktopctl open Mail && sleep 1 && desktopctl key press opt+cmd+f && sleep 0.2 && desktopctl type "shane" && desktopctl key press enter
```

```bash
desktopctl open launchpad && sleep 1 && desktopctl type "Calculator" && sleep 0.5 && desktopctl key press enter
```

```bash
desktopctl open Calculator && sleep 1 && desktopctl type "2+2" && desktopctl key press enter
```

```bash
desktopctl open Preview -- ~/Downloads/test.jpeg && \
  sleep 1 && \
  desktopctl key press ctrl+cmd+f && \
  sleep 1 && \
  desktopctl pointer drag 656 391 856 591 120 && \
  sleep 0.2 && \
  desktopctl key press cmd+k && \
  sleep 0.2 && \
  desktopctl key press cmd+s
```
