import importlib.util
import json
from pathlib import Path
import socket
import tempfile
import threading
import time
import unittest
from unittest.mock import Mock, patch

from gi.repository import Gio

spec = importlib.util.spec_from_file_location("localsend_menu", Path(__file__).with_name("localsend_menu.py"))
menu_module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(menu_module)


class FileInfo:
    def __init__(self, uri):
        self.uri = uri

    def get_uri(self):
        return self.uri


class MenuTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory(prefix="localsend-menu-tests-")
        self.addCleanup(temporary.cleanup)
        self.config = Path(temporary.name)
        directory = patch.object(menu_module, "config_directory", return_value=self.config)
        directory.start()
        self.addCleanup(directory.stop)
        idle = patch.object(menu_module.GLib, "idle_add", return_value=0)
        idle.start()
        self.addCleanup(idle.stop)

    def test_preserves_chinese_spaces_newlines_and_shell_characters(self):
        path = "/tmp/中文 folder/a\n$(touch SHOULD_NOT_EXIST).wps"
        uri = Gio.File.new_for_path(path).get_uri()
        self.assertEqual(menu_module.selected_paths([FileInfo(uri)]), [path])
        menu = menu_module.LocalSendMenu()
        with patch.object(menu_module.subprocess, "run") as run:
            menu._submit("FINGERPRINT", (path,))
        self.assertEqual(run.call_args.args[0], [menu.binary, "send", "--to", "FINGERPRINT", "--", path])
        self.assertNotIn("shell", run.call_args.kwargs)

    def test_remote_and_empty_selections_have_no_send_menu(self):
        menu = menu_module.LocalSendMenu()
        self.assertEqual(menu.get_file_items([]), [])
        self.assertEqual(menu.get_file_items([FileInfo("smb://host/share/file")]), [])

    def test_opening_a_context_menu_does_not_start_a_stopped_cli(self):
        menu = menu_module.LocalSendMenu()
        with patch.object(menu_module, "config_directory", return_value=Path("/tmp/nonexistent-localsend-menu-test")), patch.object(menu_module.threading, "Thread") as thread:
            menu.get_file_items([FileInfo("file:///tmp/folder")])
            thread.assert_not_called()

    def test_builds_a_device_submenu_and_routes_to_the_selected_identity(self):
        menu = menu_module.LocalSendMenu()
        menu._devices = [
            {"fingerprint": "ONE", "alias": "同名手机", "online": True},
            {"fingerprint": "TWO", "alias": "同名手机", "online": True},
        ]
        with patch.object(menu, "_refresh"), patch.object(menu, "_send") as send:
            top = menu.get_file_items([FileInfo("file:///tmp/folder")])
            self.assertEqual(top[0].get_property("label"), "LocalSend")
            submenu = top[0].get_property("menu")
            items = submenu.get_items()
            self.assertEqual(items[0].get_property("label"), "同名手机 · ONE")
            self.assertEqual(items[1].get_property("label"), "同名手机 · TWO")
            items[1].emit("activate")
            self.assertEqual(send.call_args.args[1:], ("TWO", ("/tmp/folder",)))

    def test_native_menu_keeps_online_and_paired_devices_only(self):
        menu = menu_module.LocalSendMenu()
        menu._running = True
        menu._last_confirmed = time.monotonic()
        menu._devices = [
            {"fingerprint": "PAIRED", "alias": "我的手机", "online": False, "paired": True},
            {"fingerprint": "STALE", "alias": "另一台手机", "online": False, "paired": False},
            {"fingerprint": "LIVE", "alias": "另一台手机", "online": True, "paired": False},
        ]
        with patch.object(menu, "_refresh"), patch.object(menu, "_ensure_poll"):
            top = menu.get_file_items([FileInfo("file:///tmp/folder")])
        items = top[0].get_property("menu").get_items()
        self.assertEqual([item.get_property("name") for item in items if item.get_property("name").startswith("LocalSend::send::")],
                         ["LocalSend::send::PAIRED", "LocalSend::send::LIVE"])
        self.assertEqual(items[1].get_property("label"), "另一台手机")
        self.assertTrue(items[1].get_property("sensitive"))

    def test_hidden_history_does_not_make_an_empty_menu_look_populated(self):
        menu = menu_module.LocalSendMenu()
        menu._devices = [{"fingerprint": "STALE", "alias": "测试接收端", "online": False}]
        with patch.object(menu, "_refresh"), patch.object(menu, "_ensure_poll"):
            top = menu.get_file_items([FileInfo("file:///tmp/folder")])
        names = [item.get_property("name") for item in top[0].get_property("menu").get_items()]
        self.assertEqual(names[0], "LocalSend::empty")
        self.assertNotIn("LocalSend::send::STALE", names)

    def test_automatic_device_reads_use_the_socket_without_starting_cli(self):
        with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as server:
            server.bind(str(self.config / "control.sock"))
            server.listen(1)
            requests = []

            def serve():
                connection, _ = server.accept()
                with connection:
                    with connection.makefile("rb") as request:
                        requests.append(json.loads(request.readline()))
                    connection.sendall(b'{"ok":true,"devices":[],"searching":false}\n')

            worker = threading.Thread(target=serve, daemon=True)
            worker.start()
            with patch.object(menu_module.subprocess, "run") as run:
                result = menu_module.query_devices("not-an-executable")
            worker.join(timeout=3)
            self.assertFalse(worker.is_alive())
            self.assertEqual(requests, [{"command": "devices"}])
            self.assertEqual(result["devices"], [])
            run.assert_not_called()

    def test_background_cache_read_does_not_schedule_idle_polling(self):
        menu = menu_module.LocalSendMenu()
        with patch.object(menu_module.GLib, "timeout_add") as schedule:
            menu._updated([], False, None)
        schedule.assert_not_called()
        with patch.object(menu, "_refresh") as refresh:
            self.assertEqual(menu._poll(), menu_module.GLib.SOURCE_REMOVE)
        refresh.assert_not_called()

    def test_stale_offline_state_is_labelled_as_unconfirmed(self):
        menu = menu_module.LocalSendMenu()
        menu._running = True
        menu._last_confirmed = 0
        menu._devices = [{"alias": "手机", "fingerprint": "ONE", "online": False, "paired": True}]
        with patch.object(menu, "_refresh"), patch.object(menu, "_ensure_poll"):
            top = menu.get_file_items([FileInfo("file:///tmp/folder")])
        item = top[0].get_property("menu").get_items()[0]
        self.assertIn("正在确认", item.get_property("label"))
        self.assertNotIn("未发现", item.get_property("label"))

    def test_live_menu_updates_do_not_rebuild_the_host_menu(self):
        menu = menu_module.LocalSendMenu()
        menu._inline = Mock(closed=False, active=True)
        with patch.object(menu_module.Nautilus.MenuProvider, "emit_items_updated_signal") as emit:
            menu._updated([], False, None)
        emit.assert_not_called()
        menu._inline.update.assert_called_once_with([], None)

    def test_a_queued_poll_cannot_discard_the_current_online_result(self):
        menu = menu_module.LocalSendMenu()
        menu._inline = Mock(closed=False, active=True)
        menu._pending_operation = "menu_poll"
        online = [{"fingerprint": "PHONE", "alias": "7的iphone", "online": True}]
        with patch.object(menu, "_refresh") as refresh:
            menu._updated(online, False, None)
        menu._inline.update.assert_called_once_with(online, None)
        refresh.assert_called_once_with(start=False, force=True, operation="menu_poll")

    def test_an_open_menu_continues_polling_after_the_old_30_second_limit(self):
        menu = menu_module.LocalSendMenu()
        (self.config / "control.sock").touch()
        menu._last_menu = 0
        menu._inline = Mock(closed=False, active=True)
        with patch.object(menu, "_refresh"):
            self.assertEqual(menu._poll(), menu_module.GLib.SOURCE_CONTINUE)
        menu._inline = None
        with patch.object(menu, "_refresh"):
            self.assertEqual(menu._poll(), menu_module.GLib.SOURCE_REMOVE)

    def test_explicit_refresh_during_an_automatic_read_is_not_lost(self):
        menu = menu_module.LocalSendMenu()
        menu._refreshing = True
        menu._refresh(start=True, operation="refresh")
        self.assertTrue(menu._pending_start)
        with patch.object(menu, "_refresh") as refresh:
            menu._updated([], False, None)
        refresh.assert_called_once_with(start=True, force=True, operation="refresh")
        self.assertFalse(menu._pending_start)

    def test_an_open_submenu_polls_locally_and_closing_stops_the_timer(self):
        menu = menu_module.LocalSendMenu()
        (self.config / "control.sock").touch()
        menu._inline = Mock(closed=False, active=True)
        with patch.object(menu_module.GLib, "timeout_add", return_value=123) as schedule:
            menu._ensure_poll()
            schedule.assert_called_once_with(400, menu._poll)
        menu._inline = None
        with patch.object(menu_module.GLib, "source_remove") as remove, patch.object(menu_module.GLib, "timeout_add", return_value=124) as schedule:
            menu._ensure_poll()
            remove.assert_called_once_with(123)
            schedule.assert_not_called()

    def test_visible_menu_polling_bypasses_the_two_second_read_throttle(self):
        menu = menu_module.LocalSendMenu()
        (self.config / "control.sock").touch()
        menu._last_refresh = time.monotonic()
        with patch.object(menu_module.threading, "Thread") as thread:
            menu._refresh(force=True)
        thread.assert_called_once()
        self.assertFalse(menu._pending_start)

    def test_timed_out_search_keeps_receiving_new_devices_without_restarting_search(self):
        menu = menu_module.LocalSendMenu()
        (self.config / "control.sock").touch()
        menu._inline = Mock(closed=False, active=True, timed_out=True)
        with patch.object(menu, "_refresh") as refresh:
            self.assertEqual(menu._poll(), menu_module.GLib.SOURCE_CONTINUE)
        refresh.assert_called_once_with(force=True, operation="menu_poll")


class InlineMenuTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.Gtk = menu_module.gtk_module()
        if not cls.Gtk.init_check():
            raise unittest.SkipTest("GTK display unavailable")

    def setUp(self):
        from gi.repository import Gio
        self.provider = Mock(_devices=[], _last_refresh=0.0, _last_confirmed=time.monotonic(), _inline=None, _running=True)
        self.submenu = Gio.Menu()
        self.submenu.append("旧设备", "app.old")
        self.model = Gio.Menu()
        self.model.append("其他菜单", "app.other")
        self.model.append_submenu("LocalSend", self.submenu)
        self.popover = self.Gtk.PopoverMenu.new_from_model(self.model)
        self.widget = menu_module.InlineDevices(self.provider, self.popover, self.submenu, ("/tmp/中文 a\nb.txt",))
        self.provider._inline = self.widget

    def tearDown(self):
        self.widget._hidden()
        self.widget._restore()

    def test_number_keys_are_scoped_to_the_visible_page_and_use_stable_identities(self):
        from gi.repository import Gdk
        devices = [{"alias":"Phone","fingerprint":"ONE","online":True},
                   {"alias":"iPad","fingerprint":"TWO","online":True}]
        self.widget.update(devices)
        with patch.object(self.widget.root, "get_mapped", return_value=True), patch.object(self.widget, "_send") as send:
            self.assertFalse(self.widget._key_pressed(None, Gdk.KEY_1, 0, 0))
            self.widget.active = True
            self.assertTrue(self.widget._key_pressed(None, Gdk.KEY_2, 0, 0))
            self.assertEqual(send.call_args.args[1], "TWO")
            self.widget.update([devices[1]])
            self.assertFalse(self.widget._key_pressed(None, Gdk.KEY_1, 0, 0))
            self.assertTrue(self.widget._key_pressed(None, Gdk.KEY_2, 0, 0))
            self.assertEqual(send.call_args.args[1], "TWO")
            self.assertFalse(self.widget._key_pressed(None,Gdk.KEY_2,0,Gdk.ModifierType.CONTROL_MASK))
            self.widget.update([{**devices[1], "online":False,"paired":True}])
            self.assertFalse(self.widget._key_pressed(None,Gdk.KEY_2,0,0))
            self.widget.active=False

    def test_restoring_a_closed_menu_is_idempotent(self):
        self.widget._restore()
        self.widget._restore()
        self.assertTrue(self.widget.closed)
        self.assertEqual(self.submenu.get_n_items(),1)

    def test_shortcuts_do_not_double_submit_while_sending(self):
        from gi.repository import Gdk
        self.widget.update([{"alias":"Phone","fingerprint":"ONE","online":True}])
        self.widget.active=True
        with patch.object(self.widget.root,"get_mapped",return_value=True), patch.object(menu_module.threading,"Thread") as thread:
            self.assertTrue(self.widget._key_pressed(None,Gdk.KEY_1,0,0))
            self.assertFalse(self.widget._key_pressed(None,Gdk.KEY_1,0,0))
            thread.assert_called_once()
        self.widget.active=False

    def test_pair_controls_target_the_same_identity_and_refresh_after_success(self):
        self.widget.update([{"alias":"Phone","fingerprint":"ONE","online":True,"paired":False}])
        button=self.widget.manage_rows.get_first_child()
        self.assertIn("加入白名单",button.get_label())
        with patch.object(self.widget,"_run_action") as action:
            button.emit("clicked")
            action.assert_called_once_with(("pair","ONE"))
        self.widget.update([{"alias":"Phone","fingerprint":"ONE","online":True,"paired":True}])
        self.assertIn("移出白名单",self.widget.manage_rows.get_first_child().get_label())
        self.widget._action_done(("pair","ONE"),"操作完成",None)
        self.provider._refresh.assert_called_with(force=True,operation="menu_poll")

    def test_refresh_stays_in_the_same_popover_and_keeps_spinning_when_empty(self):
        before = self.Gtk.Window.get_toplevels().get_n_items()
        with patch.object(self.popover, "popdown") as popdown:
            self.widget.refresh.emit("clicked")
            self.widget.update([])
            self.assertTrue(self.widget.spinner.get_spinning())
            self.assertTrue(self.widget.waiting)
            popdown.assert_not_called()
        self.assertEqual(self.Gtk.Window.get_toplevels().get_n_items(), before)
        self.provider._refresh.assert_called_once_with(start=True, operation="refresh")
        self.assertEqual(self.model.get_item_attribute_value(0, "label", None).unpack(), "其他菜单")

    def test_discovered_phone_replaces_waiting_state_without_closing(self):
        self.widget.begin_refresh()
        self.widget.update([{"alias": "7的iphone", "fingerprint": "PHONE", "online": False}])
        self.assertTrue(self.widget.spinner.get_spinning())
        self.widget.update([{"alias": "7的iphone", "fingerprint": "PHONE", "online": True}])
        self.assertFalse(self.widget.spinner.get_spinning())
        self.assertFalse(self.widget.waiting)
        button = self.widget.rows.get_first_child()
        self.assertEqual(button.get_label(), "1. 7的iphone")
        self.assertTrue(button.get_sensitive())

    def test_identical_device_snapshot_clears_the_stale_confirmation_label(self):
        device = [{"alias": "7的iphone", "fingerprint": "PHONE", "online": True}]
        self.provider._last_confirmed = 0
        self.widget.update(device)
        self.assertIn("正在确认", self.widget.rows.get_first_child().get_label())
        self.provider._last_confirmed = time.monotonic()
        self.widget.update(device)
        self.assertEqual(self.widget.rows.get_first_child().get_label(), "1. 7的iphone")

    def test_sending_uses_the_original_selection_and_identity(self):
        self.widget.update([{"alias": "Phone", "fingerprint": "PHONE", "online": True}])
        with patch.object(menu_module.threading, "Thread") as thread:
            self.widget.rows.get_first_child().emit("clicked")
        args = thread.call_args.kwargs["args"]
        self.assertEqual(args[:2], ("PHONE", ("/tmp/中文 a\nb.txt",)))
        self.assertTrue(self.widget.sending)

    def test_backend_error_stops_spinner_and_allows_retry(self):
        self.widget.begin_refresh()
        self.widget.update(None, "stopped")
        self.assertFalse(self.widget.spinner.get_spinning())
        self.assertTrue(self.widget.refresh.get_sensitive())
        self.assertIn("重试", self.widget.refresh_label.get_label())

    def test_search_timeout_stops_spinner_and_reenables_refresh(self):
        self.provider._devices = [{"alias": "Phone", "fingerprint": "PHONE", "online": False, "paired": True}]
        self.widget.begin_refresh()
        self.widget._search_timeout()
        self.assertTrue(self.widget.timed_out)
        self.assertFalse(self.widget.waiting)
        self.assertFalse(self.widget.spinner.get_spinning())
        self.assertTrue(self.widget.refresh.get_sensitive())
        self.assertIn("未找到", self.widget.hint.get_label())
        self.assertFalse(self.widget.rows.get_first_child().get_sensitive())
        self.widget.begin_refresh()
        self.assertFalse(self.widget.timed_out)
        self.assertTrue(self.widget.spinner.get_spinning())

    def test_slow_reads_still_update_the_real_device_button(self):
        with tempfile.TemporaryDirectory() as directory:
            (Path(directory) / "control.sock").touch()
            online = [{"fingerprint": "PHONE", "alias": "7的iphone", "online": True}]
            def delayed_read(*_args, **kwargs):
                if kwargs.get("operation") != "menu_close":
                    time.sleep(0.55)  # Longer than the local 400 ms poll interval.
                return {"ok": True, "devices": online, "searching": False}
            with patch.object(menu_module, "config_directory", return_value=Path(directory)), patch.object(menu_module, "query_devices", side_effect=delayed_read):
                provider = menu_module.LocalSendMenu()
                widget = menu_module.InlineDevices(provider, self.popover, self.submenu, ("/tmp/test.txt",))
                provider._inline = widget
                widget._opened()
                context = menu_module.GLib.MainContext.default()
                end = time.monotonic() + 2.0
                try:
                    while time.monotonic() < end:
                        while context.pending():
                            context.iteration(False)
                        time.sleep(0.005)
                    button = widget.rows.get_first_child()
                    self.assertIsNotNone(button, "online results were discarded behind queued polls")
                    self.assertEqual(button.get_label(), "1. 7的iphone")
                    self.assertTrue(button.get_sensitive())
                finally:
                    widget._hidden()
                    widget.closed = True
                    # Drain the last worker's callback without scheduling another read.
                    end = time.monotonic() + 0.65
                    while time.monotonic() < end:
                        while context.pending():
                            context.iteration(False)
                        time.sleep(0.005)
                    widget._restore()

    def test_the_real_refresh_timer_stops_after_four_seconds(self):
        loop = menu_module.GLib.MainLoop()
        original = self.widget._search_timeout
        elapsed = []
        started = time.monotonic()
        def expired():
            result = original()
            elapsed.append(time.monotonic() - started)
            loop.quit()
            return result
        with patch.object(self.widget, "_search_timeout", side_effect=expired):
            self.widget.begin_refresh()
            guard = menu_module.GLib.timeout_add(4600, lambda: (loop.quit(), False)[1])
            loop.run()
            if elapsed:
                menu_module.GLib.source_remove(guard)
        self.assertEqual(len(elapsed), 1)
        self.assertGreaterEqual(elapsed[0], 3.8)
        self.assertLess(elapsed[0], 4.6)
        self.assertFalse(self.widget.spinner.get_spinning())
        self.assertTrue(self.widget.refresh.get_sensitive())

    def test_disconnection_revokes_the_send_button(self):
        device = {"alias": "Phone", "fingerprint": "PHONE", "online": True, "paired": True}
        self.widget.update([device])
        self.assertTrue(self.widget.rows.get_first_child().get_sensitive())
        self.widget.update([{**device, "online": False, "checking": False}])
        self.assertIn("未连接", self.widget.rows.get_first_child().get_label())
        self.assertFalse(self.widget.rows.get_first_child().get_sensitive())

    def test_an_offline_peer_does_not_flash_confirmation_on_every_recheck(self):
        device = {"alias": "Phone", "fingerprint": "PHONE", "online": False, "paired": True, "checked": True, "checking": False}
        self.widget.update([device])
        button = self.widget.rows.get_first_child()
        self.widget.update([{**device, "checking": True}])
        self.assertIs(button, self.widget.rows.get_first_child())
        self.assertIn("未连接", button.get_label())

    def test_stopped_cli_can_still_be_started_by_clicking_a_paired_device(self):
        self.provider._running = False
        self.widget.update([{"alias": "Phone", "fingerprint": "PHONE", "online": False, "paired": True}])
        self.assertTrue(self.widget.rows.get_first_child().get_sensitive())
        self.assertIn("启动", self.widget.rows.get_first_child().get_label())

    def test_unpaired_device_appears_online_and_disappears_offline(self):
        paired = {"fingerprint": "PAIRED", "alias": "我的手机", "paired": True, "online": False}
        neighbor = {"fingerprint": "NEIGHBOR", "alias": "其他设备", "paired": False, "online": True}
        stale = {"fingerprint": "STALE", "alias": "测试接收端", "paired": False, "online": False}
        self.widget.update([paired, stale, neighbor])
        def labels():
            result = []
            row = self.widget.rows.get_first_child()
            while row is not None:
                result.append(row.get_label())
                row = row.get_next_sibling()
            return result
        self.assertEqual(labels(), ["1. 我的手机（未连接）", "2. 其他设备"])
        self.widget.update([paired, stale, {**neighbor, "online": False}])
        self.assertEqual(labels(), ["1. 我的手机（未连接）"])
        self.widget.update([paired, stale, neighbor])
        self.assertEqual(labels(), ["1. 我的手机（未连接）", "2. 其他设备"])

    def test_hidden_history_cannot_use_up_the_device_row_limit(self):
        history = [{"fingerprint": str(i), "alias": "旧测试设备", "online": False} for i in range(130)]
        self.widget.update(history + [{"fingerprint": "LIVE", "alias": "在线设备", "online": True}])
        row = self.widget.rows.get_first_child()
        self.assertEqual(row.get_label(), "1. 在线设备")
        self.assertTrue(row.get_sensitive())
        self.assertIsNone(row.get_next_sibling())

    def test_only_the_localsend_page_is_modified_and_can_be_restored(self):
        self.assertEqual(list(menu_module.local_submenus(self.model)), [self.submenu])
        self.assertIsNotNone(self.submenu.get_item_attribute_value(0, "custom", None))
        self.assertEqual(self.model.get_n_items(), 2)

    def test_a_menu_prepared_while_hidden_is_hooked_when_later_opened(self):
        with tempfile.TemporaryDirectory() as directory, patch.object(menu_module, "config_directory", return_value=Path(directory)):
            provider = menu_module.LocalSendMenu()
            model = Gio.Menu()
            local = Gio.Menu()
            provider._selections["LocalSendRefresh1"] = ("/tmp/original.txt",)
            local.append("刷新设备列表", "view.extension_LocalSendRefresh1")
            model.append_submenu("LocalSend", local)
            popover = self.Gtk.PopoverMenu.new_from_model(model)
            button = self.Gtk.MenuButton(popover=popover)
            window = self.Gtk.Window()
            window.set_child(button)
            try:
                provider._bind_inline(("/tmp/original.txt",))
                self.assertIsNotNone(popover._localsend_map_handler)
                self.assertIsNone(provider._inline)
                with patch.object(provider, "_refresh"), patch.object(provider, "_ensure_poll"):
                    provider._on_menu_map(popover)
                self.assertIs(provider._inline.popover, popover)
                self.assertEqual(provider._inline.paths, ("/tmp/original.txt",))
                provider._inline._restore()
                provider._inline = None
            finally:
                window.destroy()

    def test_only_the_visible_localsend_page_starts_network_monitoring(self):
        self.popover.set_autohide(False)
        settings = self.Gtk.Settings.get_default()
        animations = settings.get_property("gtk-enable-animations")
        settings.set_property("gtk-enable-animations", False)
        button = self.Gtk.MenuButton(popover=self.popover)
        window = self.Gtk.Window()
        window.set_child(button)
        context = menu_module.GLib.MainContext.default()
        def pump():
            end = time.monotonic() + 0.4
            while time.monotonic() < end:
                while context.pending():
                    context.iteration(False)
                time.sleep(0.005)
        try:
            window.present()
            pump()
            button.popup()
            pump()
            self.assertFalse(self.widget.root.get_mapped())
            self.assertFalse(self.widget.active)
            self.provider._refresh.assert_not_called()
            self.popover.set_property("visible-submenu", "LocalSend")
            pump()
            self.assertTrue(self.widget.root.get_mapped())
            self.assertTrue(self.widget.active)
            self.provider._refresh.assert_called_once_with(force=True, operation="menu_open")
            self.popover.set_property("visible-submenu", "main")
            pump()
            self.assertFalse(self.widget.active)
            self.provider._end_menu.assert_called_once_with(self.widget.session)
        finally:
            self.popover.popdown()
            window.destroy()
            settings.set_property("gtk-enable-animations", animations)

    def test_each_menu_uses_its_own_selection_when_two_windows_differ(self):
        with patch.object(menu_module.LocalSendMenu, "_refresh"):
            provider = menu_module.LocalSendMenu()
        provider._selections = {"LocalSendRefresh1": ("/tmp/one",), "LocalSendRefresh11": ("/tmp/two",)}
        first = Gio.Menu()
        first.append("刷新设备列表", "view.extension_LocalSendRefresh1")
        second = Gio.Menu()
        second.append("刷新设备列表", "view.extension_LocalSendRefresh11")
        self.assertEqual(provider._paths_for_submenu(first), ("/tmp/one",))
        self.assertEqual(provider._paths_for_submenu(second), ("/tmp/two",))

    def test_recreated_context_popover_is_watched_without_reselecting_files(self):
        with patch.object(menu_module.LocalSendMenu, "_refresh"):
            provider = menu_module.LocalSendMenu()
        provider._selections["LocalSendRefresh1"] = ("/tmp/kept-selection",)
        view = self.Gtk.Box()
        provider._watch_view(view)
        for _ in range(2):
            popup = self.Gtk.PopoverMenu.new_from_model(None)
            popup.set_parent(view)
            self.assertIsNotNone(popup._localsend_map_handler)
            local = Gio.Menu()
            local.append("刷新设备列表", "view.extension_extensions_0_1_LocalSendRefresh1")
            model = Gio.Menu()
            model.append_submenu("LocalSend", local)
            popup.set_menu_model(model)
            with patch.object(provider, "_refresh"), patch.object(provider, "_ensure_poll"):
                provider._on_menu_map(popup)
            self.assertEqual(provider._inline.paths, ("/tmp/kept-selection",))
            provider._inline._restore()
            provider._inline = None
            popup.unparent()


if __name__ == "__main__":
    unittest.main()
