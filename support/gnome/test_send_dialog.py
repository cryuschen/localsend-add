import importlib.util
from pathlib import Path
import time
import unittest
from unittest.mock import Mock, patch

import gi
gi.require_version('Gtk', '4.0')
from gi.repository import Gio, GLib, Gtk

spec = importlib.util.spec_from_file_location('send_dialog', Path(__file__).parent / 'localsend-transfer@local/send_dialog.py')
dialog = importlib.util.module_from_spec(spec)
spec.loader.exec_module(dialog)


class PickerTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        Gtk.init()
        cls.app = Gtk.Application(application_id='local.localsend.PickerTest', flags=Gio.ApplicationFlags.NON_UNIQUE)
        cls.app.register(None)

    def drain(self, done):
        end = time.monotonic() + 2
        while not done() and time.monotonic() < end:
            GLib.MainContext.default().iteration(False)
            time.sleep(.005)
        self.assertTrue(done())

    def setUp(self):
        self.menu = dialog.load_menu(Path(__file__).parent.parent / 'nautilus/localsend_menu.py')
        self.query = patch.object(self.menu, 'query_devices', return_value={'ok': True, 'devices': [
            {'fingerprint': 'LIVE', 'alias': '手机', 'online': True},
            {'fingerprint': 'OFF', 'alias': '平板', 'online': False, 'paired': True},
            {'fingerprint': 'STALE', 'alias': '旧设备', 'online': False},
        ]})
        self.query_mock = self.query.start()
        self.window = dialog.SendWindow(self.app, '/tmp/test-localsend', self.menu)
        self.window.present()
        self.drain(lambda: not self.window.loading and self.window.get_mapped())

    def tearDown(self):
        self.window.close_request()
        self.window.destroy()
        self.query.stop()

    def rows(self):
        rows = []
        item = self.window.rows.get_first_child()
        while item:
            rows.append(item)
            item = item.get_next_sibling()
        return rows

    def test_uses_shared_menu_session_and_filters_offline_unpaired_peers(self):
        self.assertEqual(self.query_mock.call_args.kwargs['operation'], 'menu_open')
        self.assertEqual([r.get_label() for r in self.rows()], ['手机', '平板（未连接）'])
        self.assertTrue(all(not row.get_sensitive() for row in self.rows()))
        self.window.paths = ['/tmp/file']
        self.window.render()
        self.assertTrue(self.rows()[0].get_sensitive())
        self.assertFalse(self.rows()[1].get_sensitive())
        self.window.poll()
        self.drain(lambda: not self.window.loading)
        self.assertEqual(self.query_mock.call_args.kwargs['operation'], 'menu_poll')

    def test_native_picker_accepts_files_and_folders_and_preserves_paths(self):
        for folder in (False, True):
            model = Gio.ListStore.new(Gio.File)
            model.append(Gio.File.new_for_path('/tmp/中文 folder/$(touch no).txt'))
            model.append(Gio.File.new_for_path('/tmp/another folder'))
            chooser = Mock()
            chooser.open_multiple_finish.return_value = model
            chooser.select_multiple_folders_finish.return_value = model
            chooser.open_multiple.side_effect = lambda _window, _cancel, callback: callback(chooser, None)
            chooser.select_multiple_folders.side_effect = chooser.open_multiple.side_effect
            with patch.object(dialog.Gtk, 'FileDialog', return_value=chooser):
                self.window.choose(folder)
            self.assertEqual(self.window.paths, ['/tmp/中文 folder/$(touch no).txt', '/tmp/another folder'])
            self.assertTrue(self.rows()[0].get_sensitive())

    def test_send_uses_same_cli_arguments_and_never_shell_text(self):
        self.window.paths = ['/tmp/中文 file; $(touch no)', '/tmp/folder']
        with patch.object(dialog.subprocess, 'run') as run:
            self.window.send('LIVE')
            self.drain(lambda: self.window.closed)
            self.assertEqual(run.call_args.args[0], ['/tmp/test-localsend', 'send', '--to', 'LIVE', '--', *self.window.paths])
            self.assertNotIn('shell', run.call_args.kwargs)

    def test_backend_failure_disables_stale_device_buttons(self):
        self.window.paths = ['/tmp/file']
        self.window.loaded(None, 'connection refused')
        self.assertEqual(self.rows(), [])
        self.assertIn('后台已停止', self.window.hint.get_label())

    def test_refresh_and_close_use_the_same_session(self):
        self.window.load('refresh')
        self.drain(lambda: not self.window.loading)
        self.assertEqual(self.query_mock.call_args.kwargs['operation'], 'refresh')
        self.window.close_request()
        self.drain(lambda: self.query_mock.call_args.kwargs['operation'] == 'menu_close')
        self.assertEqual(self.query_mock.call_args.kwargs['session'], self.window.session)


if __name__ == '__main__':
    unittest.main()
