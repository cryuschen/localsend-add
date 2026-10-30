# Modified by localsend-add contributors, 2026. See NOTICE and LICENSE.
"""LocalSend device submenu for GNOME Files (Nautilus 43+).

Networking and file transfer run in the CLI, never in Nautilus' UI thread.
"""

import json
import os
from pathlib import Path
import socket
import subprocess
import sys
import threading
import time

import gi

gi.require_version("Nautilus", "4.1")
from gi.repository import Gio, GLib, GObject, Nautilus

SEARCH_SECONDS = 4


def visible_devices(devices):
    """Keep trusted destinations and currently reachable LAN peers in the menu."""
    return [device for device in (devices or []) if device.get("paired") or device.get("online")]


def query_devices(binary, start=False, operation="devices", session=None, deadline=None):
    """Read the local backend off the UI thread; only an explicit action may start it."""
    if start:
        subprocess.run(
            [binary, "start"], stdin=subprocess.DEVNULL,
            capture_output=True, text=True, encoding="utf-8", timeout=18, check=True,
        )
    request = {"command": operation}
    if operation in ("menu_open", "menu_poll", "menu_close"):
        request["session"] = session
    if operation == "refresh":
        remaining = SEARCH_SECONDS if deadline is None else deadline - time.monotonic()
        if remaining <= 0:
            request = {"command": "devices"}
        else:
            request["timeout_ms"] = max(1, min(4000, int(remaining * 1000)))
    with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as connection:
        connection.settimeout(3)
        connection.connect(str(config_directory() / "control.sock"))
        connection.sendall((json.dumps(request) + "\n").encode())
        with connection.makefile("rb") as reply:
            line = reply.readline(1024 * 1024 + 1)
        if len(line) > 1024 * 1024 or not line.endswith(b"\n"):
            raise ValueError("Invalid device response size")
        data = json.loads(line)
    if not isinstance(data, dict) or not data.get("ok") or not isinstance(data.get("devices"), list):
        raise ValueError("Invalid device response")
    if not all(isinstance(device, dict) for device in data["devices"]):
        raise ValueError("Invalid device entry")
    return data


def config_directory():
    configured = os.environ.get("XDG_CONFIG_HOME", "")
    root = Path(configured) if os.path.isabs(configured) else Path.home() / ".config"
    return root / "localsend-cli"


def selected_paths(files):
    """Decode local URIs without splitting names at spaces or newlines."""
    paths = []
    for file_info in files:
        path = Gio.File.new_for_uri(file_info.get_uri()).get_path()
        if path is None:
            return []
        paths.append(path)
    return paths


def gtk_module():
    gi.require_version("Gtk", "4.0")
    from gi.repository import Gtk
    return Gtk


def local_submenus(model, seen=None):
    """Find only our page; leave all other file-manager menu sections alone."""
    if model is None:
        return
    seen = set() if seen is None else seen
    if model in seen:
        return
    seen.add(model)
    for index in range(model.get_n_items()):
        label = model.get_item_attribute_value(index, "label", None)
        submenu = model.get_item_link(index, "submenu")
        if label is not None and label.unpack().replace("_", "") == "LocalSend":
            if isinstance(submenu, Gio.Menu) and submenu.is_mutable():
                yield submenu
        for link in (submenu, model.get_item_link(index, "section")):
            yield from local_submenus(link, seen)


def widget_tree(root):
    pending = [root]
    count = 0
    while pending and count < 5000:
        widget = pending.pop()
        count += 1
        yield widget
        child = widget.get_first_child()
        while child is not None:
            pending.append(child)
            child = child.get_next_sibling()


class InlineDevices:
    """Live device buttons and spinner inside the existing GtkPopoverMenu page."""

    def __init__(self, provider, popover, submenu, paths):
        self.Gtk = Gtk = gtk_module()
        self.provider = provider
        self.popover = popover
        self.submenu = submenu
        self.paths = tuple(paths)
        self.closed = False
        self.restored = False
        self.active = False
        self.session = f"{os.getpid()}-{id(self):x}"
        self.waiting = False
        self.sending = False
        self.timed_out = False
        self._timeout_source = None
        self._deadline = None
        self._signature = None
        self._device_order = {}
        self._shortcuts = {}
        self.managing = False
        self.original = [Gio.MenuItem.new_from_model(submenu, i) for i in range(submenu.get_n_items())]
        self.root = Gtk.Box(orientation=Gtk.Orientation.VERTICAL, width_request=220)
        self.rows = Gtk.Box(orientation=Gtk.Orientation.VERTICAL)
        self.root.append(self.rows)
        self.hint = Gtk.Label(xalign=0, wrap=True, max_width_chars=29)
        self.hint.set_margin_start(12)
        self.hint.set_margin_end(12)
        self.hint.set_margin_top(4)
        self.hint.set_margin_bottom(4)
        self.hint.add_css_class("dim-label")
        self.hint.set_visible(False)
        self.root.append(self.hint)
        self.refresh = Gtk.Button()
        self.refresh.add_css_class("flat")
        box = Gtk.Box(spacing=8)
        self.spinner = Gtk.Spinner(width_request=16, height_request=16, visible=False)
        box.append(self.spinner)
        self.refresh_label = Gtk.Label(label="刷新设备列表", xalign=0, hexpand=True)
        box.append(self.refresh_label)
        self.refresh.set_child(box)
        self.refresh.connect("clicked", lambda *_: self.begin_refresh())
        self.root.append(self.refresh)
        self.manage = Gtk.MenuButton(label="白名单管理")
        self.manage.add_css_class("flat")
        self.manage_rows = Gtk.Box(orientation=Gtk.Orientation.VERTICAL)
        self.manage_popover = Gtk.Popover(child=self.manage_rows)
        self.manage.set_popover(self.manage_popover)
        self.root.append(self.manage)
        self.key_controller = Gtk.EventControllerKey()
        self.key_controller.set_propagation_phase(Gtk.PropagationPhase.CAPTURE)
        self.key_controller.connect("key-pressed", self._key_pressed)
        self.popover.add_controller(self.key_controller)
        actions = []
        if self.paths:
            actions.append(("通过浏览器分享所选项目", ("web", "share", "--", *self.paths)))
        actions += [("开启浏览器上传", ("web", "receive")), ("查看浏览器链接", ("web", "status")),
                    ("关闭浏览器链接", ("web", "stop")), ("取消当前传输", ("cancel",))]
        for text, arguments in actions:
            action = Gtk.Button(label=text)
            action.add_css_class("flat")
            action.connect("clicked", lambda _button, args=arguments: self._run_action(args))
            self.root.append(action)
        slot_id = f"localsend-inline-{id(self):x}"
        slot = Gio.MenuItem.new(None, None)
        slot.set_attribute_value("custom", GLib.Variant("s", slot_id))
        submenu.remove_all()
        submenu.append_item(slot)
        if not popover.add_child(self.root, slot_id):
            submenu.remove_all()
            for item in self.original:
                submenu.append_item(item)
            raise RuntimeError("The current file-manager menu cannot host inline controls")
        self._closed_handler = popover.connect("closed", self._closed)
        self.update(provider._devices)
        # A mapped context popover is not necessarily the LocalSend page.
        # GTK maps this custom child only while its submenu page is visible.
        self.root.connect("map", self._opened)
        self.root.connect("unmap", self._hidden)
        if self.root.get_mapped():
            GLib.idle_add(self._opened)

    def _opened(self, *_args):
        if self.closed or self.active:
            return GLib.SOURCE_REMOVE
        self.active = True
        self.timed_out = False
        self.provider._refresh(force=True, operation="menu_open")
        self.provider._ensure_poll()
        return GLib.SOURCE_REMOVE

    def _hidden(self, *_args):
        if not self.active:
            return
        self.active = False
        self._stop_waiting()
        self.provider._end_menu(self.session)
        self.provider._ensure_poll()

    def begin_refresh(self):
        if self.closed or self.sending or self.managing:
            return
        self.waiting = True
        self.timed_out = False
        self._deadline = time.monotonic() + SEARCH_SECONDS
        if self._timeout_source is not None:
            GLib.source_remove(self._timeout_source)
        self._timeout_source = GLib.timeout_add(SEARCH_SECONDS * 1000, self._search_timeout)
        self.spinner.set_visible(True)
        self.spinner.start()
        self.refresh_label.set_label("正在查找设备…")
        self.refresh.set_sensitive(False)
        self.hint.set_label(f"请保持手机 LocalSend 打开，最多查找 {SEARCH_SECONDS} 秒。")
        self.hint.set_visible(True)
        self.provider._last_refresh = 0.0
        self.provider._last_menu = time.monotonic()
        self.provider._refresh(start=True, operation="refresh")
        self.provider._ensure_poll()

    def _stop_waiting(self):
        self.waiting = False
        if self._timeout_source is not None:
            GLib.source_remove(self._timeout_source)
            self._timeout_source = None
        self.spinner.stop()
        self.spinner.set_visible(False)
        self.refresh.set_sensitive(True)

    def _search_timeout(self):
        self._timeout_source = None
        if self.closed or not self.waiting:
            return GLib.SOURCE_REMOVE
        self.timed_out = True
        self._stop_waiting()
        self.refresh_label.set_label("重新查找设备")
        self.hint.set_label("未找到在线设备，请打开手机 LocalSend 后重试。")
        self.hint.set_visible(True)
        self.update([{**device, "online": False, "checking": False} for device in self.provider._devices])
        return GLib.SOURCE_REMOVE

    def update(self, devices, error=None):
        if self.closed or self.sending or self.managing:
            return
        if error:
            self._stop_waiting()
            self.refresh_label.set_label("查找失败，点击重试")
            self.hint.set_label("后台可能已停止，请点击重试。")
            self.hint.set_visible(True)
            self.rows.set_sensitive(False)
            return
        devices = visible_devices(devices)
        self.rows.set_sensitive(True)
        online = any(device.get("online") for device in devices)
        if online:
            self.timed_out = False
            self._stop_waiting()
            self.refresh_label.set_label("刷新设备列表")
            self.hint.set_visible(False)
        fresh = time.monotonic() - self.provider._last_confirmed <= 2
        # The same device snapshot can change from cached to confirmed. That
        # transition must redraw the row even when its online flag is identical.
        signature = ([(d.get("fingerprint"), d.get("alias"), bool(d.get("online")), bool(d.get("paired")), bool(d.get("checking") and not d.get("checked"))) for d in devices], self.waiting, fresh, self.timed_out, bool(self.provider._running))
        if signature == self._signature:
            return
        self._signature = signature
        while self.rows.get_first_child() is not None:
            self.rows.remove(self.rows.get_first_child())
        self._shortcuts = {}
        while self.manage_rows.get_first_child() is not None:
            self.manage_rows.remove(self.manage_rows.get_first_child())
        for device in devices[:128]:
            fingerprint = device.get("fingerprint")
            if isinstance(fingerprint, str) and fingerprint not in self._device_order:
                self._device_order[fingerprint] = len(self._device_order) + 1
        devices.sort(key=lambda device: self._device_order.get(device.get("fingerprint"), 100000))
        aliases = [device.get("alias") for device in devices]
        for device in devices[:128]:
            fingerprint = device.get("fingerprint")
            if not isinstance(fingerprint, str) or not fingerprint:
                continue
            alias = str(device.get("alias") or "未命名设备").replace("\n", " ").replace("\r", " ")
            if aliases.count(device.get("alias")) > 1:
                alias += " · " + fingerprint[-6:]
            if self.timed_out and not device.get("online"):
                alias += "（未连接）"
            elif not self.provider._running:
                alias += "（点击启动并发送）"
            elif not self.waiting and not fresh:
                alias += "（正在确认…）"
            elif not device.get("online"):
                alias += "（查找中…）" if self.waiting else ("（确认中…）" if device.get("checking") and not device.get("checked") and not self.timed_out else "（未连接）")
            number = self._device_order.get(fingerprint, 0)
            prefix = f"{number}. " if self.paths and 1 <= number <= 9 else ""
            button = self.Gtk.Button(label=prefix + alias)
            button.add_css_class("flat")
            button.set_sensitive(bool(self.paths) and ((fresh and bool(device.get("online"))) or (not self.provider._running and not self.waiting)))
            child = button.get_child()
            if isinstance(child, self.Gtk.Label):
                child.set_xalign(0)
            button.connect("clicked", self._send, fingerprint)
            self.rows.append(button)
            if self.paths and 1 <= number <= 9:
                self._shortcuts[number] = (fingerprint, button)
            paired = bool(device.get("paired"))
            management = self.Gtk.Button(label=("移出白名单：" if paired else "加入白名单：") + alias)
            management.add_css_class("flat")
            management.connect("clicked", lambda _button, fp=fingerprint, trusted=paired: self._run_action(("unpair" if trusted else "pair", fp)))
            self.manage_rows.append(management)
        self.manage.set_sensitive(bool(devices))

    def _key_pressed(self, _controller, keyval, _keycode, state):
        from gi.repository import Gdk
        if (self.closed or not self.active or not self.root.get_mapped()
                or self.sending or self.managing or self.manage_popover.get_visible()):
            return False
        if state & (Gdk.ModifierType.CONTROL_MASK | Gdk.ModifierType.ALT_MASK | Gdk.ModifierType.SUPER_MASK):
            return False
        number = Gdk.keyval_to_unicode(keyval)
        if not ord("1") <= number <= ord("9"):
            return False
        selected = self._shortcuts.get(number - ord("0"))
        if selected is None or not self.rows.get_sensitive() or not selected[1].get_sensitive():
            return False
        self._send(selected[1], selected[0])
        return True

    def _run_action(self, arguments):
        if self.closed or self.managing or self.sending:
            return
        self.managing = True
        self.rows.set_sensitive(False)
        self.manage.set_sensitive(False)
        self.manage_popover.popdown()
        def work():
            try:
                result = subprocess.run([self.provider.binary, *arguments], stdin=subprocess.DEVNULL,
                    capture_output=True, text=True, encoding="utf-8", timeout=20, check=True)
                output, error = result.stdout, None
            except (OSError, subprocess.SubprocessError) as failure:
                output, error = "", str(failure)
                if isinstance(failure, subprocess.CalledProcessError):
                    error = failure.stderr.strip() or error
            GLib.idle_add(self._action_done, arguments, output, error)
        threading.Thread(target=work, daemon=True).start()

    def _action_done(self, arguments, output, error):
        self.managing = False
        if self.closed:
            return GLib.SOURCE_REMOVE
        self.rows.set_sensitive(True)
        self.manage.set_sensitive(True)
        self._signature = None
        self.provider._refresh(force=True, operation="menu_poll")
        if error:
            self.hint.set_label(error)
            self.hint.set_visible(True)
        elif arguments[0] == "web":
            # Only an explicit browser-link action opens this native dialog.
            dialog = self.Gtk.AlertDialog(message="LocalSend 浏览器链接", detail=output.strip())
            parent = self.popover.get_root()
            dialog.show(parent if isinstance(parent, self.Gtk.Window) else None)
        else:
            self.hint.set_label(output.strip() or "操作完成")
            self.hint.set_visible(True)
        return GLib.SOURCE_REMOVE

    def _send(self, _button, fingerprint):
        if self.sending or self.managing or self.closed:
            return
        self.sending = True
        self.rows.set_sensitive(False)
        self.refresh.set_sensitive(False)
        self.spinner.set_visible(True)
        self.spinner.start()
        self.refresh_label.set_label("正在提交发送…")
        threading.Thread(target=self.provider._submit, args=(fingerprint, self.paths, self._submitted), daemon=True).start()

    def _submitted(self, error):
        if self.closed:
            return GLib.SOURCE_REMOVE
        self.sending = False
        if error:
            self.rows.set_sensitive(True)
            self.update(None, error)
        else:
            self.popover.popdown()
        return GLib.SOURCE_REMOVE

    def _closed(self, *_args):
        self._hidden()
        self.closed = True
        self._stop_waiting()
        if self.provider._inline is self:
            self.provider._inline = None
            self.provider._ensure_poll()
        # Restore after the host finishes closing its menu, never during a click.
        GLib.idle_add(self._restore)

    def _restore(self):
        if self.restored:
            return GLib.SOURCE_REMOVE
        self.restored = True
        self.closed = True
        self._stop_waiting()
        self.popover.disconnect(self._closed_handler)
        self.popover.remove_controller(self.key_controller)
        # Window destruction may already have unparented the custom child.
        if self.root.get_parent() is not None:
            self.popover.remove_child(self.root)
        self.submenu.remove_all()
        for item in self.original:
            self.submenu.append_item(item)
        return GLib.SOURCE_REMOVE


class LocalSendMenu(GObject.GObject, Nautilus.MenuProvider):
    def __init__(self):
        super().__init__()
        self.binary = os.environ.get(
            "LOCALSEND_MENU_BINARY", str(Path.home() / ".local/bin/localsend-cli")
        )
        self._devices = self._paired_devices()
        self._refreshing = False
        self._last_refresh = 0.0
        self._last_menu = 0.0
        self._error = None
        self._searching = False
        self._running = False
        self._last_confirmed = 0.0
        self._poll_source = None
        self._poll_interval = None
        self._inline = None
        self._menu_serial = 0
        self._selections = {}
        self._pending_start = False
        self._pending_operation = None
        # Warm the menu cache when Files loads the extension. This never starts
        # a stopped backend; opening a menu should not have to use an old result.
        self._refresh()

    @staticmethod
    def _paired_devices():
        try:
            data = json.loads((config_directory() / "paired-v2.json").read_text())
            return [
                {"fingerprint": fingerprint, "alias": value["alias"],
                 "paired": True, "online": False}
                for fingerprint, value in data.get("devices", {}).items()
            ]
        except (OSError, ValueError, KeyError, TypeError):
            return []

    def get_file_items(self, files):
        paths = selected_paths(files)
        if not paths:
            return []
        return self._menu_items(paths)

    def get_background_items(self, current_folder):
        return self._menu_items(())

    def _menu_items(self, paths):
        self._last_menu = time.monotonic()
        self._refresh()
        self._ensure_poll()
        self._menu_serial += 1
        refresh_name = f"LocalSendRefresh{self._menu_serial}"
        self._selections[refresh_name] = tuple(paths)
        if len(self._selections) > 128:
            del self._selections[next(iter(self._selections))]
        parent = Nautilus.MenuItem(
            name="LocalSend::devices", label="LocalSend", icon="localsend_app"
        )
        submenu = Nautilus.Menu()
        devices = visible_devices(self._devices)
        aliases = [device.get("alias", "") for device in devices]
        for device in devices[:128]:
            fingerprint = device.get("fingerprint")
            if not isinstance(fingerprint, str) or not fingerprint:
                continue
            alias = str(device.get("alias") or "未命名设备").replace("\n", " ").replace("\r", " ")
            if aliases.count(device.get("alias", "")) > 1:
                alias += " · " + fingerprint[-6:]
            if self._running:
                if time.monotonic() - self._last_confirmed > 5:
                    alias += "（正在确认…）"
                elif not device.get("online", False):
                    alias += "（未发现）"
            item = Nautilus.MenuItem(
                name="LocalSend::send::" + fingerprint,
                label=alias,
                tip="将选中的项目发送到此设备",
                icon="smartphone" if device.get("paired") else "computer",
            )
            item.set_property("sensitive", bool(paths) and (not self._running or (
                time.monotonic() - self._last_confirmed <= 2 and bool(device.get("online")))))
            item.connect("activate", self._send, fingerprint, tuple(paths))
            submenu.append_item(item)
        if not devices:
            label = "无法启动 LocalSend，点击重试" if self._error else (
                "正在查找设备…" if self._refreshing or self._searching else "启动并查找设备…"
            )
            placeholder = Nautilus.MenuItem(name="LocalSend::empty", label=label)
            placeholder.set_property("sensitive", not (self._refreshing or self._searching))
            if not (self._refreshing or self._searching):
                placeholder.connect("activate", self._retry, tuple(paths))
            submenu.append_item(placeholder)
        refresh = Nautilus.MenuItem(
            name=refresh_name, label="刷新设备列表", icon="view-refresh"
        )
        refresh.connect("activate", self._retry, tuple(paths))
        submenu.append_item(refresh)
        management = Nautilus.MenuItem(name="LocalSend::manage", label="白名单管理")
        manage_menu = Nautilus.Menu()
        for device in devices[:128]:
            fp = device.get("fingerprint")
            if not isinstance(fp, str) or not fp:
                continue
            command = "unpair" if device.get("paired") else "pair"
            item = Nautilus.MenuItem(name="LocalSend::trust::" + fp,
                label=("移出白名单：" if command == "unpair" else "加入白名单：") + str(device.get("alias") or "未命名设备"))
            item.connect("activate", self._trust, command, fp)
            manage_menu.append_item(item)
        management.set_submenu(manage_menu)
        submenu.append_item(management)
        parent.set_submenu(submenu)
        GLib.idle_add(self._bind_inline, tuple(paths), self._menu_serial, 0)
        return [parent]

    def _paths_for_submenu(self, model):
        for index in range(model.get_n_items()):
            action = model.get_item_attribute_value(index, "action", None)
            if action is not None:
                # Nautilus prefixes extension action names, keeping ASCII names.
                token = action.unpack().rsplit(".", 1)[-1].rsplit("_", 1)[-1]
                if token in self._selections:
                    return self._selections[token]
        return None

    def _bind_inline(self, paths, serial=None, attempt=0):
        if serial is not None and serial != self._menu_serial:
            return GLib.SOURCE_REMOVE
        if self._inline is not None and not self._inline.closed:
            return GLib.SOURCE_REMOVE
        Gtk = gtk_module()
        windows = Gtk.Window.get_toplevels()
        watched = False
        for index in range(windows.get_n_items()):
            for widget in widget_tree(windows.get_item(index)):
                if widget.__gtype__.name == "NautilusFilesView":
                    self._watch_view(widget)
                    watched = True
                if not isinstance(widget, Gtk.PopoverMenu):
                    continue
                for submenu in local_submenus(widget.get_menu_model()):
                    selected = self._paths_for_submenu(submenu)
                    if selected is None:
                        continue
                    widget._localsend_paths = selected
                    watched = True
                    # Nautilus may prepare the menu on selection, long before
                    # it is opened. Bind on map as well, rather than timing out.
                    self._watch_popover(widget)
                    if widget.get_visible():
                        self._on_menu_map(widget)
                        if self._inline is not None:
                            return GLib.SOURCE_REMOVE
        if watched:
            return GLib.SOURCE_REMOVE
        if attempt < 10:
            GLib.timeout_add(50, self._bind_inline, paths, serial, attempt + 1)
        return GLib.SOURCE_REMOVE

    def _watch_view(self, view):
        # Nautilus 50 recreates its context popover on *every* right click,
        # including when the selection has not changed. Observe only that
        # view's direct children, not the entire desktop/widget hierarchy.
        if getattr(view, "_localsend_children_model", None) is not None:
            return
        model = view.observe_children()
        view._localsend_children_model = model
        model.connect("items-changed", self._view_children_changed)

    def _view_children_changed(self, model, position, removed, added):
        Gtk = gtk_module()
        for index in range(position, position + added):
            widget = model.get_item(index)
            if isinstance(widget, Gtk.PopoverMenu):
                self._watch_popover(widget)

    def _watch_popover(self, popover):
        if not getattr(popover, "_localsend_map_handler", None):
            popover._localsend_map_handler = popover.connect("map", self._on_menu_map)

    def _on_menu_map(self, popover):
        if self._inline is not None and not self._inline.closed:
            return
        for submenu in local_submenus(popover.get_menu_model()):
            paths = self._paths_for_submenu(submenu)
            if paths is None:
                continue
            try:
                self._inline = InlineDevices(self, popover, submenu, paths)
            except RuntimeError as error:
                print(f"LocalSend inline menu: {error}", file=sys.stderr)
                continue
            self._refresh(force=True)
            self._ensure_poll()
            if os.environ.get("LOCALSEND_MENU_DEBUG") == "1":
                print("LocalSend: live refresh attached inside the existing context menu", file=sys.stderr, flush=True)
            return

    def _retry(self, _item, paths):
        # Normally the custom refresh button handles this without activating a
        # menu action. This fallback also attempts to bind the current menu.
        self._bind_inline(tuple(paths))
        if self._inline is not None:
            self._inline.begin_refresh()
            return
        self._last_refresh = 0.0
        self._last_menu = time.monotonic()
        self._refresh(start=True, operation="refresh")
        self._ensure_poll()

    def _ensure_poll(self):
        active = self._inline is not None and self._inline.active and not self._inline.closed
        if self._poll_source is not None and not active:
            GLib.source_remove(self._poll_source)
            self._poll_source = None
        if self._poll_source is None and active:
            # Only local IPC is polled at 400 ms; the backend limits network
            # checks to once per five seconds for each known device.
            self._poll_source = GLib.timeout_add(400, self._poll)
            self._poll_interval = 400

    def _refresh(self, start=False, force=False, operation="devices"):
        if not start and not (config_directory() / "control.sock").exists():
            self._running = False
            self._last_confirmed = 0.0
            self._devices = self._paired_devices()
            if self._inline is not None:
                self._inline.update(self._devices)
            return
        if self._refreshing:
            self._pending_start |= start
            # An explicit search or opening the submenu must not be lost behind
            # a cache read; don't let ordinary polls replace these actions.
            priority = {"devices": 0, "menu_poll": 1, "menu_open": 2, "refresh": 3}
            if priority[operation] > priority.get(self._pending_operation, -1):
                self._pending_operation = operation
            return
        if operation == "devices" and not (start or force) and time.monotonic() - self._last_refresh < 2:
            return
        self._refreshing = True
        self._last_refresh = time.monotonic()
        session = self._inline.session if self._inline is not None else None
        deadline = self._inline._deadline if self._inline is not None and operation == "refresh" else None
        threading.Thread(target=self._load_devices, args=(start, operation, session, deadline), daemon=True).start()

    def _load_devices(self, start=False, operation="devices", session=None, deadline=None):
        try:
            data = query_devices(self.binary, start=start, operation=operation, session=session, deadline=deadline)
            # Starting from a refresh skips MenuOpen; register the visible page
            # too so it receives five-second checks after the search finishes.
            if start and session is not None and operation == "refresh":
                query_devices(self.binary, operation="menu_open", session=session)
            GLib.idle_add(self._updated, data["devices"], bool(data.get("searching")), None)
        except (OSError, ValueError, subprocess.SubprocessError) as error:
            GLib.idle_add(self._updated, None, False, str(error))

    def _end_menu(self, session):
        def close():
            try:
                query_devices(self.binary, operation="menu_close", session=session)
            except (OSError, ValueError, subprocess.SubprocessError):
                pass
        threading.Thread(target=close, daemon=True).start()

    def _updated(self, devices, searching, error):
        changed = devices != self._devices or error != self._error or not self._running
        if devices is not None:
            self._devices = devices
            self._running = True
            self._last_confirmed = time.monotonic()
        self._refreshing = False
        self._searching = searching
        self._error = error
        # A slow read may finish after the next timer tick queued another poll.
        # Render this completed snapshot before starting that poll; otherwise
        # a steady backlog can leave the visible rows stale indefinitely.
        if self._inline is not None and not self._inline.closed:
            self._inline.update(devices, error)
        elif changed and self._pending_operation is None:
            Nautilus.MenuProvider.emit_items_updated_signal(self)
        if self._pending_operation is not None:
            operation, start = self._pending_operation, self._pending_start
            self._pending_operation, self._pending_start = None, False
            if operation == "refresh" or (self._inline is not None and self._inline.active):
                self._refresh(start=start, force=True, operation=operation)
        self._ensure_poll()
        return GLib.SOURCE_REMOVE

    def _poll(self):
        if self._inline is None or not self._inline.active or self._inline.closed:
            self._poll_source = None
            return GLib.SOURCE_REMOVE
        if not (config_directory() / "control.sock").exists():
            self._running = False
            self._last_confirmed = 0.0
            self._devices = self._paired_devices()
            self._inline.update(self._devices)
        else:
            self._refresh(force=True, operation="menu_poll")
        return GLib.SOURCE_CONTINUE

    def _trust(self, _item, command, fingerprint):
        def work():
            try:
                subprocess.run([self.binary, command, fingerprint], stdin=subprocess.DEVNULL,
                    capture_output=True, text=True, encoding="utf-8", timeout=20, check=True)
                GLib.idle_add(lambda: self._refresh(force=True))
            except (OSError, subprocess.SubprocessError) as failure:
                print("LocalSend 白名单操作失败：" + str(failure), file=sys.stderr)
        threading.Thread(target=work, daemon=True).start()

    def _send(self, _item, fingerprint, paths):
        threading.Thread(
            target=self._submit, args=(fingerprint, paths), daemon=True
        ).start()

    def _submit(self, fingerprint, paths, completion=None):
        error = None
        try:
            # A list of arguments, never shell text: names may contain arbitrary punctuation.
            subprocess.run(
                [self.binary, "send", "--to", fingerprint, "--", *paths],
                stdin=subprocess.DEVNULL, capture_output=True, text=True,
                encoding="utf-8", timeout=18, check=True,
            )
        except (OSError, subprocess.SubprocessError) as failure:
            error = str(failure)
            if completion is None:
                print("LocalSend 发送请求失败：" + error, file=sys.stderr)
        if completion is not None:
            GLib.idle_add(completion, error)
