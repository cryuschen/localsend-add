"""On-demand native picker. Discovery and sending use the Nautilus backend API."""
import argparse
import importlib.util
from pathlib import Path
import subprocess
import threading
import uuid

import gi
gi.require_version('Gtk', '4.0')
from gi.repository import Gio, GLib, Gtk


def load_menu(path=None):
    path = Path(path) if path else Path(GLib.get_user_data_dir()) / 'nautilus-python/extensions/localsend_menu.py'
    spec = importlib.util.spec_from_file_location('localsend_shared_menu', path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


class SendWindow(Gtk.ApplicationWindow):
    def __init__(self, app, binary, menu):
        super().__init__(application=app, title='LocalSend · 发送', default_width=420)
        self.binary, self.menu = binary, menu
        self.session = 'panel-' + str(uuid.uuid4())
        self.paths, self.devices, self.order = [], [], {}
        self.closed = self.loading = self.sending = False
        self.cancel = Gio.Cancellable()
        self.connect('close-request', self.close_request)
        box = Gtk.Box(orientation=Gtk.Orientation.VERTICAL, spacing=14)
        for side in ('top', 'bottom', 'start', 'end'):
            getattr(box, 'set_margin_' + side)(20)
        self.set_child(box)
        choices = Gtk.Box(spacing=8)
        for text, folder in [('选择文件', False), ('选择文件夹', True)]:
            button = Gtk.Button(label=text)
            button.connect('clicked', lambda _button, folder=folder: self.choose(folder))
            choices.append(button)
        box.append(choices)
        self.selection = Gtk.Label(label='尚未选择文件', xalign=0, wrap=True, max_width_chars=42)
        box.append(self.selection)
        box.append(Gtk.Label(label='发送到', xalign=0))
        self.rows = Gtk.Box(orientation=Gtk.Orientation.VERTICAL, spacing=5)
        box.append(self.rows)
        self.hint = Gtk.Label(label='正在查找设备…', xalign=0, wrap=True, max_width_chars=42)
        box.append(self.hint)
        self.refresh = Gtk.Button(label='刷新设备列表')
        self.refresh.connect('clicked', lambda *_: self.load('refresh'))
        box.append(self.refresh)
        self.poll_source = GLib.timeout_add(400, self.poll)
        self.load('menu_open')

    def choose(self, folder=False):
        dialog = Gtk.FileDialog(title='选择要发送的文件夹' if folder else '选择要发送的文件')
        def selected(source, result):
            try:
                model = source.select_multiple_folders_finish(result) if folder else source.open_multiple_finish(result)
                paths = [model.get_item(i).get_path() for i in range(model.get_n_items())]
                if not all(paths):
                    self.hint.set_label('请选择本机文件或文件夹。')
                    return
                self.paths = list(dict.fromkeys(paths))
                self.selection.set_label('\n'.join(Path(path).name for path in self.paths))
                self.render()
            except GLib.Error as error:
                if not self.closed and not error.matches(Gtk.dialog_error_quark(), Gtk.DialogError.DISMISSED):
                    self.hint.set_label(error.message)
        if folder:
            dialog.select_multiple_folders(self, self.cancel, selected)
        else:
            dialog.open_multiple(self, self.cancel, selected)

    def load(self, operation):
        if self.closed or self.loading or self.sending:
            return
        self.loading = True
        if operation == 'refresh':
            self.hint.set_label('正在查找设备…')
        def work():
            try:
                data = self.menu.query_devices(self.binary, operation=operation, session=self.session)
                error = None
            except (OSError, ValueError, subprocess.SubprocessError) as failure:
                data, error = None, str(failure)
            GLib.idle_add(self.loaded, data, error)
        threading.Thread(target=work, daemon=True).start()

    def loaded(self, data, error):
        self.loading = False
        if self.closed:
            return GLib.SOURCE_REMOVE
        if error:
            self.devices = []
            self.hint.set_label('后台已停止或暂时无法连接，请从顶栏重新启动。')
        else:
            self.devices = self.menu.visible_devices(data['devices'])
            self.hint.set_label('选择文件后，点击在线设备发送。' if any(d.get('online') for d in self.devices)
                                else '请保持对方 LocalSend 打开，并连接同一局域网。')
        self.render()
        return GLib.SOURCE_REMOVE

    def render(self):
        while self.rows.get_first_child() is not None:
            self.rows.remove(self.rows.get_first_child())
        for device in self.devices:
            fingerprint = device.get('fingerprint')
            if not isinstance(fingerprint, str) or not fingerprint:
                continue
            self.order.setdefault(fingerprint, len(self.order))
        aliases = [d.get('alias') for d in self.devices]
        for device in sorted(self.devices[:128], key=lambda d: self.order.get(d.get('fingerprint'), 999)):
            fingerprint = device.get('fingerprint')
            if not isinstance(fingerprint, str) or not fingerprint:
                continue
            alias = str(device.get('alias') or '未命名设备').replace('\n', ' ').replace('\r', ' ')
            if aliases.count(device.get('alias')) > 1:
                alias += ' · ' + fingerprint[-6:]
            if not device.get('online'):
                alias += '（未连接）'
            button = Gtk.Button(label=alias)
            button.set_sensitive(bool(self.paths) and bool(device.get('online')) and not self.sending)
            button.connect('clicked', lambda _button, target=fingerprint: self.send(target))
            self.rows.append(button)

    def send(self, fingerprint):
        if self.sending or self.closed or not self.paths:
            return
        self.sending = True
        self.refresh.set_sensitive(False)
        self.render()
        paths = tuple(self.paths)
        def work():
            try:
                subprocess.run([self.binary, 'send', '--to', fingerprint, '--', *paths],
                               stdin=subprocess.DEVNULL, capture_output=True, text=True,
                               encoding='utf-8', timeout=18, check=True)
                error = None
            except (OSError, subprocess.SubprocessError) as failure:
                error = failure.stderr.strip() if isinstance(failure, subprocess.CalledProcessError) else str(failure)
            GLib.idle_add(self.sent, error)
        threading.Thread(target=work, daemon=True).start()

    def sent(self, error):
        if self.closed:
            return GLib.SOURCE_REMOVE
        self.sending = False
        if error:
            self.hint.set_label(error)
            self.refresh.set_sensitive(True)
            self.render()
        else:
            self.close()
        return GLib.SOURCE_REMOVE

    def poll(self):
        if self.closed:
            return GLib.SOURCE_REMOVE
        self.load('menu_poll')
        return GLib.SOURCE_CONTINUE

    def close_request(self, *_):
        if self.closed:
            return False
        self.closed = True
        self.cancel.cancel()
        GLib.source_remove(self.poll_source)
        def release():
            try:
                self.menu.query_devices(self.binary, operation='menu_close', session=self.session)
            except (OSError, ValueError, subprocess.SubprocessError):
                pass
        threading.Thread(target=release, daemon=True).start()
        return False


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--binary', required=True)
    args = parser.parse_args()
    menu = load_menu()
    app = Gtk.Application(application_id='local.localsend.SendPicker', flags=Gio.ApplicationFlags.NON_UNIQUE)
    def activate(application):
        window = SendWindow(application, args.binary, menu)
        window.present()
        GLib.idle_add(lambda: (window.choose(), GLib.SOURCE_REMOVE)[1])
    app.connect('activate', activate)
    app.run([])


if __name__ == '__main__':
    main()
