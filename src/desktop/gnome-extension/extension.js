// DesktopCtl GNOME Shell extension (GNOME 50, Wayland).
//
// Exposes Mutter window management and a panel-menu control surface to the
// DesktopCtl daemon over a private session D-Bus interface.
//
// Interface: sh.desktopctl.Shell at /sh/desktopctl/Shell.

import GObject from 'gi://GObject';
import Gio from 'gi://Gio';
import GLib from 'gi://GLib';
import Meta from 'gi://Meta';
import Shell from 'gi://Shell';
import St from 'gi://St';

import * as Main from 'resource:///org/gnome/shell/ui/main.js';
import * as PanelMenu from 'resource:///org/gnome/shell/ui/panelMenu.js';
import * as PopupMenu from 'resource:///org/gnome/shell/ui/popupMenu.js';
import {Extension} from 'resource:///org/gnome/shell/extensions/extension.js';

const BUS_NAME = 'sh.desktopctl.Shell';
const OBJECT_PATH = '/sh/desktopctl/Shell';

// D-Bus interface definition. The daemon is the sole consumer.
const IFACE_XML = `
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
</node>`;

// Wraps the Mutter window registry and the D-Bus service.
class ShellService {
    constructor() {
        this._dbusImpl = Gio.DBusExportedObject.wrapJSObject(IFACE_XML, this);

        // Stable opaque string IDs <-> Meta.Window objects.
        this._idToWindow = new Map();
        this._windowToId = new Map();
        this._nextId = 1;

        // Per-window destroy handler tracking for cleanup.
        this._windowSignals = new Map();

        this._displaySignals = [];
        this._ownerId = 0;

        this.controlEnabled = false;
    }

    export() {
        this._dbusImpl.export(Gio.DBus.session, OBJECT_PATH);
        this._ownerId = Gio.bus_own_name(
            Gio.BusType.SESSION,
            BUS_NAME,
            Gio.BusNameOwnerFlags.NONE,
            null,
            null,
            null);
        this._connectDisplaySignals();
    }

    destroy() {
        this._disconnectDisplaySignals();

        for (const [win, ids] of this._windowSignals) {
            for (const id of ids) {
                try {
                    win.disconnect(id);
                } catch (e) {
                    // window may already be gone
                }
            }
        }
        this._windowSignals.clear();
        this._idToWindow.clear();
        this._windowToId.clear();

        if (this._ownerId) {
            Gio.bus_unown_name(this._ownerId);
            this._ownerId = 0;
        }

        try {
            this._dbusImpl.unexport();
        } catch (e) {
            // not exported
        }
    }

    // --- ID management ------------------------------------------------------

    _idFor(win) {
        let id = this._windowToId.get(win);
        if (id !== undefined)
            return id;

        id = `w${this._nextId++}`;
        this._windowToId.set(win, id);
        this._idToWindow.set(id, win);

        // Drop the mapping when the window is destroyed.
        const destroyId = win.connect('unmanaged', () => this._forget(win));
        let ids = this._windowSignals.get(win);
        if (!ids) {
            ids = [];
            this._windowSignals.set(win, ids);
        }
        ids.push(destroyId);

        return id;
    }

    _forget(win) {
        const id = this._windowToId.get(win);
        if (id !== undefined) {
            this._idToWindow.delete(id);
            this._windowToId.delete(win);
        }
        const ids = this._windowSignals.get(win);
        if (ids) {
            for (const sid of ids) {
                try {
                    win.disconnect(sid);
                } catch (e) {
                    // already disconnected
                }
            }
            this._windowSignals.delete(win);
        }
        this._emitWindowsChanged();
    }

    _windowById(id) {
        const win = this._idToWindow.get(id);
        if (win && !win.get_compositor_private?.()) {
            // Window appears destroyed; clean up and report missing.
            this._forget(win);
            return null;
        }
        return win || null;
    }

    // --- Serialization ------------------------------------------------------

    _serialize(win, stackingIndex) {
        const rect = win.get_frame_rect();
        const tracker = Shell.WindowTracker.get_default();
        const app = tracker.get_window_app(win);
        const ws = win.get_workspace();

        const bounds = new GLib.Variant('a{sv}', {
            x: GLib.Variant.new_int32(rect.x),
            y: GLib.Variant.new_int32(rect.y),
            width: GLib.Variant.new_int32(rect.width),
            height: GLib.Variant.new_int32(rect.height),
        });

        const states = [];
        if (win.minimized)
            states.push('minimized');
        const maximized = win.get_maximized?.() ?? 0;
        if (maximized === Meta.MaximizeFlags.BOTH)
            states.push('maximized');
        else if (maximized & Meta.MaximizeFlags.HORIZONTAL)
            states.push('maximized-horizontal');
        else if (maximized & Meta.MaximizeFlags.VERTICAL)
            states.push('maximized-vertical');
        if (win.is_fullscreen?.())
            states.push('fullscreen');
        if (win.is_above?.())
            states.push('above');

        const dict = {
            id: GLib.Variant.new_string(this._idFor(win)),
            title: GLib.Variant.new_string(win.get_title() ?? ''),
            app_id: GLib.Variant.new_string(
                app ? (app.get_id() ?? '') : ''),
            pid: GLib.Variant.new_int32(win.get_pid?.() ?? -1),
            bounds: bounds,
            workspace: GLib.Variant.new_int32(ws ? ws.index() : -1),
            stacking: GLib.Variant.new_int32(stackingIndex),
            focused: GLib.Variant.new_boolean(win.has_focus?.() ?? false),
            states: new GLib.Variant('as', states),
        };

        return dict;
    }

    _normalWindowsInStackOrder() {
        // get_window_actors() returns bottom-to-top stacking order.
        const actors = global.get_window_actors();
        const out = [];
        actors.forEach((actor, index) => {
            const win = actor.get_meta_window();
            if (!win)
                return;
            const type = win.get_window_type?.();
            if (type !== undefined && type !== Meta.WindowType.NORMAL &&
                type !== Meta.WindowType.DIALOG &&
                type !== Meta.WindowType.MODAL_DIALOG)
                return;
            out.push({win, index});
        });
        return out;
    }

    // --- D-Bus methods ------------------------------------------------------

    ListWindows() {
        const entries = this._normalWindowsInStackOrder();
        return entries.map(({win, index}) => this._serialize(win, index));
    }

    GetActiveWindow() {
        const win = global.display.get_focus_window?.() ?? null;
        if (!win)
            return {};
        // Find its stacking index for consistency.
        const entries = this._normalWindowsInStackOrder();
        const match = entries.find(e => e.win === win);
        return this._serialize(win, match ? match.index : -1);
    }

    ActivateWindow(id) {
        const win = this._windowById(id);
        if (!win)
            return false;
        const time = global.get_current_time();
        if (win.minimized)
            win.unminimize();
        win.activate(time);
        return true;
    }

    CloseWindow(id) {
        const win = this._windowById(id);
        if (!win)
            return false;
        win.delete(global.get_current_time());
        return true;
    }

    MoveResizeWindow(id, x, y, width, height) {
        const win = this._windowById(id);
        if (!win)
            return false;
        if (win.maximized_horizontally || win.maximized_vertically)
            win.unmaximize(Meta.MaximizeFlags.BOTH);
        // user_op = true so the placement is honored.
        win.move_resize_frame(true, x, y, width, height);
        return true;
    }

    SetWorkspace(id, workspace) {
        const win = this._windowById(id);
        if (!win)
            return false;
        const wm = global.workspace_manager;
        if (workspace >= wm.get_n_workspaces())
            return false;
        win.change_workspace_by_index(workspace, false);
        return true;
    }

    // --- Signals ------------------------------------------------------------

    _emitWindowsChanged() {
        try {
            this._dbusImpl.emit_signal('WindowsChanged', null);
        } catch (e) {
            // not exported yet
        }
    }

    emitControlToggled(enabled) {
        this.controlEnabled = enabled;
        try {
            this._dbusImpl.emit_signal(
                'ControlToggled', new GLib.Variant('(b)', [enabled]));
        } catch (e) {
            // not exported yet
        }
    }

    _connectDisplaySignals() {
        const display = global.display;
        this._displaySignals.push([
            display,
            display.connect('window-created',
                () => this._emitWindowsChanged()),
        ]);
        this._displaySignals.push([
            display,
            display.connect('grab-op-end',
                () => this._emitWindowsChanged()),
        ]);

        const wm = global.workspace_manager;
        this._displaySignals.push([
            wm,
            wm.connect('active-workspace-changed',
                () => this._emitWindowsChanged()),
        ]);

        // Restack changes (z-order) come through the stage's restacked signal.
        this._displaySignals.push([
            display,
            display.connect('restacked',
                () => this._emitWindowsChanged()),
        ]);
    }

    _disconnectDisplaySignals() {
        for (const [obj, sigId] of this._displaySignals) {
            try {
                obj.disconnect(sigId);
            } catch (e) {
                // gone
            }
        }
        this._displaySignals = [];
    }
}

// Top-bar indicator mirroring the macOS/Windows tray surfaces.
const DesktopCtlIndicator = GObject.registerClass(
class DesktopCtlIndicator extends PanelMenu.Button {
    _init(service) {
        super._init(0.0, 'DesktopCtl');

        this._service = service;

        const icon = new St.Icon({
            icon_name: 'input-tablet-symbolic',
            style_class: 'system-status-icon',
        });
        this.add_child(icon);

        const menu = this.menu;

        // Daemon status (label only).
        this._statusItem = new PopupMenu.PopupMenuItem(
            'Daemon: connected', {reactive: false});
        menu.addMenuItem(this._statusItem);

        menu.addMenuItem(new PopupMenu.PopupSeparatorMenuItem());

        // Enable agent control toggle -> emits ControlToggled.
        this._controlSwitch = new PopupMenu.PopupSwitchMenuItem(
            'Enable agent control', false);
        this._controlSwitch.connect('toggled', (_item, state) => {
            this._service.emitControlToggled(state);
            log(`[desktopctl] agent control ${state ? 'enabled' : 'disabled'}`);
        });
        menu.addMenuItem(this._controlSwitch);

        menu.addMenuItem(new PopupMenu.PopupSeparatorMenuItem());

        // Permissions submenu.
        const perms = new PopupMenu.PopupSubMenuMenuItem('Permissions');
        const capItem = new PopupMenu.PopupMenuItem('Re-trigger capture consent');
        capItem.connect('activate',
            () => log('[desktopctl] capture consent requested'));
        perms.menu.addMenuItem(capItem);
        const inputItem = new PopupMenu.PopupMenuItem('Re-trigger input consent');
        inputItem.connect('activate',
            () => log('[desktopctl] input consent requested'));
        perms.menu.addMenuItem(inputItem);
        menu.addMenuItem(perms);

        // Journal toggle + open output dir.
        this._journalSwitch = new PopupMenu.PopupSwitchMenuItem(
            'Journal', false);
        this._journalSwitch.connect('toggled', (_item, state) =>
            log(`[desktopctl] journal ${state ? 'on' : 'off'}`));
        menu.addMenuItem(this._journalSwitch);

        const openDir = new PopupMenu.PopupMenuItem('Open output directory');
        openDir.connect('activate',
            () => log('[desktopctl] open output directory'));
        menu.addMenuItem(openDir);

        menu.addMenuItem(new PopupMenu.PopupSeparatorMenuItem());

        // App policy mode submenu.
        const policy = new PopupMenu.PopupSubMenuMenuItem('App policy');
        const modes = [
            ['allow-all', 'Allow all'],
            ['allow-only-selected', 'Allow only selected'],
            ['allow-all-except', 'Allow all except'],
        ];
        this._policyItems = [];
        this._policyMode = 'allow-all';
        for (const [key, label] of modes) {
            const item = new PopupMenu.PopupMenuItem(label);
            item.setOrnament?.(
                key === this._policyMode
                    ? PopupMenu.Ornament.DOT
                    : PopupMenu.Ornament.NONE);
            item.connect('activate', () => this._setPolicy(key));
            policy.menu.addMenuItem(item);
            this._policyItems.push([key, item]);
        }
        menu.addMenuItem(policy);

        menu.addMenuItem(new PopupMenu.PopupSeparatorMenuItem());

        // Quick links.
        const settings = new PopupMenu.PopupMenuItem('Settings…');
        settings.connect('activate',
            () => log('[desktopctl] open settings'));
        menu.addMenuItem(settings);

        const recent = new PopupMenu.PopupMenuItem('Recent requests…');
        recent.connect('activate',
            () => log('[desktopctl] open recent requests'));
        menu.addMenuItem(recent);
    }

    _setPolicy(key) {
        this._policyMode = key;
        for (const [k, item] of this._policyItems) {
            item.setOrnament?.(
                k === key ? PopupMenu.Ornament.DOT : PopupMenu.Ornament.NONE);
        }
        log(`[desktopctl] app policy set to ${key}`);
    }
});

export default class DesktopCtlExtension extends Extension {
    enable() {
        this._service = new ShellService();
        this._service.export();

        this._indicator = new DesktopCtlIndicator(this._service);
        Main.panel.addToStatusArea(
            'desktopctl', this._indicator, 0, 'right');
    }

    disable() {
        if (this._indicator) {
            this._indicator.destroy();
            this._indicator = null;
        }
        if (this._service) {
            this._service.destroy();
            this._service = null;
        }
    }
}
