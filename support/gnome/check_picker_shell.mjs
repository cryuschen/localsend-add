// Run only in an isolated headless GNOME session (see test_picker_shell.sh).
import Gio from 'gi://Gio';
import GLib from 'gi://GLib';
import Clutter from 'gi://Clutter';
import Shell from 'gi://Shell';
import St from 'gi://St';
import * as Main from 'resource:///org/gnome/shell/ui/main.js';
import * as Scripting from 'resource:///org/gnome/shell/ui/scripting.js';

const pause = ms => new Promise(resolve => GLib.timeout_add(GLib.PRIORITY_DEFAULT, ms, () => {
    resolve(); return GLib.SOURCE_REMOVE;
}));
let checks = 0;
function check(condition, message) { if (!condition) throw new Error(message); checks++; }
async function until(predicate, message) {
    for (let i = 0; i < 80; i++) { if (predicate()) return; await pause(100); }
    throw new Error(message);
}
const windows = () => global.get_window_actors().map(actor => actor.meta_window);

export async function run() {
    await Scripting.disableHelperAutoExit();
    const config = GLib.get_user_config_dir() + '/localsend-cli';
    GLib.mkdir_with_parents(config, 0o700);
    const requests = [];
    const service = new Gio.SocketService();
    service.add_address(new Gio.UnixSocketAddress({path: config + '/control.sock'}),
        Gio.SocketType.STREAM, Gio.SocketProtocol.DEFAULT, null);
    service.connect('incoming', (_service, connection) => {
        const input = new Gio.DataInputStream({base_stream: connection.get_input_stream()});
        input.read_line_async(GLib.PRIORITY_DEFAULT, null, (stream, result) => {
            try {
                const [line] = stream.read_line_finish_utf8(result);
                const request = JSON.parse(line); requests.push(request);
                const response = JSON.stringify({ok: true, devices: [
                    {fingerprint: 'TEST-LIVE', alias: '测试手机', online: true, paired: false},
                    {fingerprint: 'TEST-OFF', alias: '测试平板', online: false, paired: true},
                ]}) + '\n';
                connection.get_output_stream().write_all(new TextEncoder().encode(response), null);
            } finally { connection.close(null); }
        });
        return true;
    });
    service.start();
    await pause(1000);
    const directory = Gio.File.new_for_uri(import.meta.url).get_parent();
    const extensionDir = directory.get_child('localsend-transfer@local');
    let extension = Main.extensionManager.lookup('localsend-transfer@local');
    if (!extension) {
        extension = Main.extensionManager.createExtensionObject('localsend-transfer@local', extensionDir, 2);
        await Main.extensionManager.loadExtension(extension);
        await Main.extensionManager._callExtensionEnable(extension.uuid);
    }
    check(extension?.stateObj, 'extension loads');
    const e = extension.stateObj;
    e.enabled = false; e.generation++;
    if (e.timer) GLib.Source.remove(e.timer);
    e.timer = 0; e.monitor?.cancel(); await pause(100);
    e.enabled = true; e.schedule = () => {}; e.lifecycle = null;
    e.binary = GLib.getenv('LS_TEST_CLI');
    e.render({ok: true, running: true, pid: 42});
    Main.overview.hide(); await pause(500);
    const pointer = global.stage.context.get_backend().get_default_seat()
        .create_virtual_device(Clutter.InputDeviceType.POINTER_DEVICE);
    async function clickWindow(x, y) {
        const frame = windows().find(window => window.get_title() === 'LocalSend · 发送').get_frame_rect();
        pointer.notify_absolute_motion(GLib.get_monotonic_time(), frame.x + x, frame.y + y);
        pointer.notify_button(GLib.get_monotonic_time(), Clutter.BUTTON_PRIMARY, Clutter.ButtonState.PRESSED);
        await pause(40);
        pointer.notify_button(GLib.get_monotonic_time(), Clutter.BUTTON_PRIMARY, Clutter.ButtonState.RELEASED);
        await pause(250);
    }
    const screenshot = new Shell.Screenshot();
    async function capture(name) {
        const image = Gio.File.new_for_path(GLib.getenv('LS_UI_OUTPUT') + '/' + name + '.png');
        const output = image.replace(null, false, Gio.FileCreateFlags.REPLACE_DESTINATION, null);
        await screenshot.screenshot(false, output); output.close(null);
    }
    const [x, y] = e.transfer.ring.get_transformed_position();
    const [width, height] = e.transfer.ring.get_transformed_size();
    pointer.notify_absolute_motion(GLib.get_monotonic_time(), x + width / 2, y + height / 2);
    pointer.notify_button(GLib.get_monotonic_time(), Clutter.BUTTON_PRIMARY, Clutter.ButtonState.PRESSED);
    await pause(40);
    pointer.notify_button(GLib.get_monotonic_time(), Clutter.BUTTON_PRIMARY, Clutter.ButtonState.RELEASED);
    await until(() => e.picker && windows().some(window => window.get_title() === 'LocalSend · 发送'), 'ring opens Rust window');
    const pid = e.picker.get_identifier();
    check(GLib.file_read_link('/proc/' + pid + '/exe') === GLib.getenv('LS_TEST_PICKER'), 'ring executes Rust binary directly');
    e.chooseFiles(); check(e.picker.get_identifier() === pid, 'duplicate activation keeps one picker');
    await pause(400);
    check(!windows().some(window => window.get_title() === '选择要发送的文件'), 'entry offers files, folders and text before opening a chooser');
    await capture('rust-picker');
    await clickWindow(70, 76);
    await until(() => windows().some(window => window.get_title() === '选择要发送的文件'), 'native file chooser opens');
    windows().find(window => window.get_title() === '选择要发送的文件').delete(global.get_current_time());
    await until(() => !windows().some(window => window.get_title() === '选择要发送的文件'), 'native chooser dismisses');
    check(windows().some(window => window.get_title() === 'LocalSend · 发送'), 'dismissal retains device selection window');
    await until(() => requests.some(request => request.command === 'menu_poll'), 'Rust polls the shared control protocol');
    await pause(500);
    await clickWindow(270, 76);
    await capture('rust-picker-text-empty');
    const text = 'LocalSend 中文文本测试\n第二行：你好🙂\n';
    St.Clipboard.get_default().set_text(St.ClipboardType.CLIPBOARD, text);
    await pause(150);
    await clickWindow(95, 298);
    await capture('rust-picker-text');
    await clickWindow(190, 390);
    await until(() => requests.some(request => request.command === 'send'), 'actual device click submits text');
    const sent = requests.find(request => request.command === 'send');
    check(sent.target === 'TEST-LIVE' && sent.paths.length === 0 && sent.text === text,
        'real pointer paste and device clicks preserve multiline Chinese text without file paths');
    await until(() => !e.picker, 'Rust process exits after window closes');
    const opened = requests.find(request => request.command === 'menu_open');
    check(opened && requests.some(request => request.command === 'menu_close' && request.session === opened.session),
        'window releases the exact discovery session');
    // The already-loaded v12 Shell entry must exec into the same Rust binary.
    const bridge = GLib.getenv('LS_TEST_BRIDGE') ?? extensionDir.get_child('send_dialog.py').get_path();
    const legacy = Gio.Subprocess.new(['/usr/bin/python3', bridge, '--binary', e.binary], Gio.SubprocessFlags.NONE);
    await until(() => windows().some(window => window.get_title() === 'LocalSend · 发送'), 'legacy entry opens Rust window');
    check(GLib.file_read_link('/proc/' + legacy.get_identifier() + '/exe') === GLib.getenv('LS_TEST_PICKER'),
        'legacy entry replaces Python rather than retaining an interpreter process');
    await pause(400);
    await clickWindow(70, 76);
    await until(() => windows().some(window => window.get_title() === '选择要发送的文件'), 'legacy file button opens native chooser');
    windows().find(window => window.get_title() === '选择要发送的文件').delete(global.get_current_time());
    await pause(200);
    windows().find(window => window.get_title() === 'LocalSend · 发送').delete(global.get_current_time());
    await new Promise((resolve, reject) => legacy.wait_check_async(null, (process, result) => {
        try { process.wait_check_finish(result); resolve(); } catch (error) { reject(error); }
    }));
    check(requests.filter(request => request.command === 'menu_open').length === 2
        && requests.filter(request => request.command === 'menu_close').length === 2, 'both entry paths clean up');
    e.disable(); service.stop(); service.close();
    print(`PASS: ${checks} native ring-to-Rust launch, text paste/send, chooser, legacy entry and cleanup checks`);
}
