# DesktopCtl GNOME Shell Extension

UUID: `desktopctl@desktopctl.sh` — Target: **Ubuntu 26.04 / GNOME 50 (Wayland)**.

GNOME has no generic external window-management API. This extension is the
sanctioned escape hatch: it runs inside the Shell process (full Mutter access)
and exposes window enumeration/operations plus a panel-menu control surface to
the `desktopctld` daemon over a **private session D-Bus interface**. The daemon
is the sole intended consumer.

## D-Bus Interface

* Bus name: `sh.desktopctl.Shell`
* Object path: `/sh/desktopctl/Shell`
* Interface: `sh.desktopctl.Shell`

```xml
<node>
  <interface name="sh.desktopctl.Shell">
    <method name="ListWindows">
      <arg type="aa{sv}" direction="out" name="windows"/>
    </method>
    <method name="GetActiveWindow">
      <arg type="a{sv}" direction="out" name="window"/>
    </method>
    <method name="ActivateWindow">
      <arg type="s" direction="in" name="id"/>
      <arg type="b" direction="out" name="ok"/>
    </method>
    <method name="CloseWindow">
      <arg type="s" direction="in" name="id"/>
      <arg type="b" direction="out" name="ok"/>
    </method>
    <method name="MoveResizeWindow">
      <arg type="s" direction="in" name="id"/>
      <arg type="i" direction="in" name="x"/>
      <arg type="i" direction="in" name="y"/>
      <arg type="u" direction="in" name="width"/>
      <arg type="u" direction="in" name="height"/>
      <arg type="b" direction="out" name="ok"/>
    </method>
    <method name="SetWorkspace">
      <arg type="s" direction="in" name="id"/>
      <arg type="u" direction="in" name="workspace"/>
      <arg type="b" direction="out" name="ok"/>
    </method>
    <signal name="WindowsChanged"/>
    <signal name="ControlToggled">
      <arg type="b" name="enabled"/>
    </signal>
  </interface>
</node>
```

### Window record (`a{sv}`)

Each entry returned by `ListWindows` / `GetActiveWindow` is a dictionary:

| Key         | Type        | Notes                                              |
| ----------- | ----------- | -------------------------------------------------- |
| `id`        | `s`         | Stable opaque window ID (DesktopCtl `window_ref`). |
| `title`     | `s`         | `Meta.Window.get_title()`                          |
| `app_id`    | `s`         | `Shell.WindowTracker` app id, else empty.          |
| `pid`       | `i`         | `-1` if unknown.                                   |
| `bounds`    | `a{sv}`     | `{x, y, width, height}` from `get_frame_rect()`.   |
| `workspace` | `i`         | Workspace index, `-1` if none.                     |
| `stacking`  | `i`         | Z-order index (bottom-to-top).                     |
| `focused`   | `b`         | `has_focus()`                                      |
| `states`    | `as`        | e.g. `minimized`, `maximized`, `fullscreen`.       |

### Stable IDs

IDs are opaque strings (`w1`, `w2`, …) issued on first sight of a window and
reused across calls. They are dropped when the window emits `unmanaged`, which
also triggers a `WindowsChanged` signal.

### Signals

* `WindowsChanged` — emitted on `window-created`, `restacked`, `grab-op-end`,
  workspace change, and window removal.
* `ControlToggled(b enabled)` — emitted when the panel-menu "Enable agent
  control" switch changes.

## Panel Menu

Top-bar indicator (analog to the macOS/Windows tray):

* Daemon status label.
* Enable-agent-control toggle (emits `ControlToggled`).
* Permissions submenu (re-trigger capture/input portal consent).
* Journal toggle + open output dir.
* App policy mode submenu (allow-all / allow-only-selected / allow-all-except).
* Quick links: Settings, Recent requests.

Most menu actions are currently stubs that log via `log()`; the daemon wires
them up in a later phase.

## Install

```sh
mkdir -p ~/.local/share/gnome-shell/extensions/desktopctl@desktopctl.sh
cp -r src/desktop/gnome-extension/* \
   ~/.local/share/gnome-shell/extensions/desktopctl@desktopctl.sh/
# Log out / back in (Wayland), then:
gnome-extensions enable desktopctl@desktopctl.sh
```

Inspect logs with `journalctl --user -f /usr/bin/gnome-shell`.

## Files

* `metadata.json` — extension manifest (`shell-version: ["50"]`).
* `extension.js` — ESM module: D-Bus service + panel indicator.
* `stylesheet.css` — optional indicator styling.
