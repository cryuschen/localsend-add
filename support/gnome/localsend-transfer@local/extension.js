import Clutter from 'gi://Clutter';
import Gio from 'gi://Gio';
import GLib from 'gi://GLib';
import St from 'gi://St';
import {Extension} from 'resource:///org/gnome/shell/extensions/extension.js';
import * as Main from 'resource:///org/gnome/shell/ui/main.js';
import * as PanelMenu from 'resource:///org/gnome/shell/ui/panelMenu.js';
import * as PopupMenu from 'resource:///org/gnome/shell/ui/popupMenu.js';
import {rpc} from './rpc.mjs';
import {COLORS, Ring} from './ring.mjs';

const nowMs = () => GLib.get_monotonic_time() / 1000;
const removeSource = id => { if (id && GLib.MainContext.default().find_source_by_id(id)) GLib.Source.remove(id); };

export class RingPanel {
    constructor(extension) {
        this.extension = extension; this.motion = new Ring();
        this.indicator = new PanelMenu.Button(0, 'LocalSend', true);
        this.indicator.add_style_class_name('ls-ring-group');
        this.row = new St.BoxLayout({style_class: 'ls-ring-row'});
        this.ring = new St.Button({style_class: 'ls-ring-button', can_focus: true,
            button_mask: St.ButtonMask.ONE | St.ButtonMask.THREE,
            y_align: Clutter.ActorAlign.CENTER, accessible_name: 'LocalSend：启动后台'});
        this.drawing = new St.DrawingArea({style_class: 'ls-ring-drawing', y_align: Clutter.ActorAlign.CENTER});
        this.drawing.connect('repaint', area => this.paint(area)); this.ring.set_child(this.drawing);
        this.ring.connect('clicked', (_actor, button) => button === 3 ? this.showMenu(true) : this.activate());
        this.ring.connect('key-press-event', (_actor, event) => {
            if (event.get_key_symbol() === Clutter.KEY_Menu
                || event.get_key_symbol() === Clutter.KEY_F10 && event.get_state() & Clutter.ModifierType.SHIFT_MASK) {
                this.showMenu(true); return Clutter.EVENT_STOP;
            }
            return Clutter.EVENT_PROPAGATE;
        });
        this.row.add_child(this.ring);
        this.decision = new St.BoxLayout({style_class: 'ls-ring-row', visible: false});
        this.peer = new St.Label({y_align: Clutter.ActorAlign.CENTER}); this.decision.add_child(this.peer);
        for (const [text, accept] of [['接收', true], ['拒绝', false]]) {
            const button = new St.Button({label: text, can_focus: true, style_class: 'ls-ring-action', y_align: Clutter.ActorAlign.CENTER});
            button.connect('clicked', () => extension.answer(this.motion.pending?.id, accept)); this.decision.add_child(button);
        }
        this.row.add_child(this.decision); this.indicator.add_child(this.row);
        this.indicator.setMenu(new PopupMenu.PopupMenu(this.ring, 0, St.Side.TOP));
        this.indicator._clickGesture.set_enabled(false);
        Main.panel.addToStatusArea('localsend-transfer', this.indicator, -1, 'left');
        this.settings = St.Settings.get();
        this.settingsSignal = this.settings.connect('notify::enable-animations', () => this.tick()); this.tick();
    }
    update(snapshot, lifecycle, time = nowMs(), wall = Date.now()) {
        this.motion.ingest(snapshot, lifecycle, time, wall);
        this.decision.visible = Boolean(this.motion.pending); this.peer.text = this.motion.pending?.alias ?? '';
        for (const child of this.decision.get_children().slice(1)) child.reactive = !this.extension.answering;
        if (this.menuMode === 'send' && this.motion.phase !== 'idle') this.indicator.menu.close();
        this.tick(); this.extension.layout();
    }
    activate() {
        const phase = this.motion.phase;
        if (phase === 'off') this.extension.start();
        else if (phase === 'idle' && !this.motion.pending) this.showMenu(false);
        else if (phase === 'transfer') this.extension.cancel(this.motion.current);
        else if (phase === 'error') { this.motion.restore(nowMs()); this.tick(); }
    }
    showMenu(context) {
        if (!context && (this.motion.phase !== 'idle' || this.motion.pending)) return;
        this.menuMode = context ? 'exit' : 'send'; this.indicator.menu.removeAll();
        const item = new PopupMenu.PopupMenuItem(context ? '退出' : '发送');
        item.setSensitive(context ? this.motion.online && !this.extension.stopping : !this.extension.picker);
        item.connect('activate', () => context ? this.extension.stop() : this.extension.chooseFiles());
        this.indicator.menu.addMenuItem(item); this.indicator.menu.open();
    }
    tick() {
        removeSource(this.animationTimer); this.animationTimer = 0;
        this.frame = this.motion.sample(nowMs(), this.settings.enable_animations);
        const names = {off: '启动后台', idle: '发送文件，右键退出', starting: '后台正在启动', stopping: '后台正在退出',
            transfer: '取消本次传输', error: '传输失败或取消，单击恢复绿色', success: '传输成功', restoring: '正在恢复绿色'};
        this.ring.accessible_name = `LocalSend：${names[this.frame.phase]}`; this.drawing.queue_repaint();
        if (this.frame.next !== null) this.animationTimer = GLib.timeout_add(GLib.PRIORITY_DEFAULT,
            Math.max(1, Math.ceil(this.frame.next)), () => { this.animationTimer = 0; this.tick(); return GLib.SOURCE_REMOVE; });
    }
    paint(area) {
        const [width, height] = area.get_surface_size(); const context = area.get_context();
        const diameter = Math.min(width, height);
        try {
            context.setLineWidth(diameter * 0.08);
            for (const layer of this.frame?.layers ?? this.motion.layers()) {
                if (layer.end <= 0) continue;
                context.setSourceRGB(...COLORS[layer.color]); context.newPath();
                context.arc(width / 2, height / 2, diameter * 0.42, -Math.PI / 2, -Math.PI / 2 + Math.PI * 2 * Math.min(1, layer.end));
                if (layer.end >= 1) context.closePath(); // Closed full strokes avoid a seam.
                context.stroke();
            }
        } finally { context.$dispose(); }
    }
    destroy() {
        removeSource(this.animationTimer); this.animationTimer = 0;
        this.settings.disconnect(this.settingsSignal); this.indicator.destroy();
    }
}

export default class LocalSendRing extends Extension {
    enable() {
        this.enabled = true; this.requests = new Set(); this.generation = (this.generation ?? 0) + 1;
        this.config = GLib.build_filenamev([GLib.get_user_config_dir(), 'localsend-cli']);
        this.pathToSocket = GLib.build_filenamev([this.config, 'control.sock']);
        this.lifecycleFile = Gio.File.new_for_path(GLib.build_filenamev([this.config, 'lifecycle.json']));
        this.binary = GLib.build_filenamev([GLib.get_home_dir(), '.local', 'bin', 'localsend-cli']);
        this.transfer = new RingPanel(this);
        this.panelHeightSignal = Main.panel.connect('notify::height', () => this.layout());
        this.watch(); this.poll();
    }
    watch() {
        this.monitor?.cancel();
        const directory = Gio.File.new_for_path(this.config); const exists = directory.query_exists(null);
        this.monitor = (exists ? directory : Gio.File.new_for_path(GLib.get_user_config_dir())).monitor_directory(Gio.FileMonitorFlags.WATCH_MOVES, null);
        this.monitor.connect('changed', (_monitor, file, other) => {
            const relevant = [file, other].some(f => ['lifecycle.json', 'control.sock', 'localsend-cli'].includes(f?.get_basename()));
            if (relevant && this.enabled) {
                if (!exists && directory.query_exists(null)) this.watch();
                this.refreshLifecycle();
            }
        });
    }
    layout() {
        const container = this.transfer?.indicator.container; if (!container) return;
        const left = Main.panel._leftBox; const vitals = Main.panel.statusArea.vitalsMenu?.container;
        if (vitals?.get_parent() === left) {
            const children = left.get_children();
            if (children.indexOf(container) !== children.indexOf(vitals) + 1) left.set_child_above_sibling(container, vitals);
        }
        container.height = Main.panel.height; this.transfer.indicator.height = Main.panel.height;
    }
    schedule(delay) {
        if (!this.enabled) return; removeSource(this.timer);
        this.timer = GLib.timeout_add(GLib.PRIORITY_DEFAULT, Math.max(10, Math.ceil(delay)), () => {
            this.timer = 0; this.poll(); return GLib.SOURCE_REMOVE;
        });
    }
    async request(message) {
        const cancel = new Gio.Cancellable(); this.requests.add(cancel);
        const timeout = GLib.timeout_add(GLib.PRIORITY_DEFAULT, 2200, () => { cancel.cancel(); return GLib.SOURCE_REMOVE; });
        try { return await rpc(this.pathToSocket, message, cancel); }
        finally { removeSource(timeout); this.requests.delete(cancel); }
    }
    async readLifecycle() {
        const cancel = new Gio.Cancellable(); this.requests.add(cancel);
        try {
            return await new Promise(resolve => this.lifecycleFile.load_contents_async(cancel, (file, result) => {
                try {
                    const [, bytes] = file.load_contents_finish(result);
                    resolve(bytes.length <= 8192 ? JSON.parse(new TextDecoder().decode(bytes)) : null);
                } catch (_) { resolve(null); }
            }));
        } finally { this.requests.delete(cancel); }
    }
    async refreshLifecycle() {
        if (this.readingLifecycle) { this.lifecycleAgain = true; return; }
        this.readingLifecycle = true;
        const generation = this.generation;
        try {
            const lifecycle = await this.readLifecycle();
            if (!this.enabled || generation !== this.generation || !lifecycle) return;
            const ended = ['stopped', 'failed'].includes(lifecycle.phase)
                && (!this.snapshot?.pid || lifecycle.pid === this.snapshot.pid);
            // Do not wait for a socket RPC timeout before showing a slow start
            // or shutdown. The owning daemon writes completion after cleanup.
            this.render(ended ? null : this.snapshot, lifecycle);
        } finally {
            this.readingLifecycle = false;
            if (this.enabled) {
                this.schedule(10);
                if (this.lifecycleAgain) { this.lifecycleAgain = false; this.refreshLifecycle(); }
            }
        }
    }
    render(snapshot, lifecycle = this.lifecycle) {
        this.snapshot = snapshot; this.lifecycle = lifecycle; this.transfer.update(snapshot, lifecycle);
        const active = this.transfer.motion.phase !== 'idle' && this.transfer.motion.phase !== 'off';
        this.schedule(active || this.transfer.motion.pending ? 200 : 1000);
    }
    async poll() {
        if (!this.enabled) return;
        if (this.polling) { this.schedule(100); return; }
        this.polling = true; const generation = this.generation;
        try {
            const [response, lifecycle] = await Promise.allSettled([this.request({command: 'panel'}), this.readLifecycle()]);
            if (!this.enabled || generation !== this.generation) return;
            if (response.status === 'fulfilled' && response.value.ok) this.render(response.value, lifecycle.value);
            else {
                const error = response.reason;
                const gone = !Gio.File.new_for_path(this.pathToSocket).query_exists(null)
                    || error?.matches?.(Gio.io_error_quark(), Gio.IOErrorEnum.CONNECTION_REFUSED);
                this.render(gone ? null : this.snapshot, lifecycle.value); // Timeout is not proof of exit.
            }
        } finally { this.polling = false; if (this.enabled && !this.timer) this.schedule(200); }
    }
    async answer(id, accept) {
        if (!id || this.answering) return; this.answering = true; this.generation++;
        try {
            const snapshot = await this.request({command: 'answer_transfer', session_id: id, accept});
            if (!snapshot.ok) throw new Error(snapshot.error);
            if (this.enabled) this.render(snapshot);
        } catch (error) { if (this.enabled) Main.notifyError('LocalSend', error.message); }
        finally { this.answering = false; if (this.enabled) this.schedule(10); }
    }
    async cancel(transfer) {
        if (!transfer || this.cancelling) return; this.cancelling = true; this.generation++;
        try {
            const snapshot = await this.request({command: 'cancel_transfer', session_id: transfer.id, direction: transfer.direction});
            if (!snapshot.ok) throw new Error(snapshot.error);
            if (this.enabled) this.render(snapshot);
        } catch (error) { if (this.enabled) Main.notifyError('LocalSend', error.message); }
        finally { this.cancelling = false; if (this.enabled) this.schedule(10); }
    }
    async run(argv) {
        const process = Gio.Subprocess.new(argv, Gio.SubprocessFlags.STDOUT_PIPE | Gio.SubprocessFlags.STDERR_PIPE);
        const cancel = new Gio.Cancellable(); this.requests.add(cancel);
        const timeout = GLib.timeout_add(GLib.PRIORITY_DEFAULT, 15000, () => { cancel.cancel(); process.force_exit(); return GLib.SOURCE_REMOVE; });
        try {
            const [, stdout, stderr] = await new Promise((resolve, reject) => process.communicate_utf8_async(null, cancel,
                (source, result) => { try { resolve(source.communicate_utf8_finish(result)); } catch (error) { reject(error); } }));
            if (!process.get_successful()) throw new Error(stderr.trim() || stdout.trim() || 'LocalSend 操作失败');
        } finally { removeSource(timeout); this.requests.delete(cancel); }
    }
    async start() {
        if (this.starting || this.transfer.motion.phase !== 'off') return;
        this.starting = true; this.transfer.motion.beginLifecycle('starting', nowMs()); this.transfer.tick();
        try { await this.run([this.binary, 'start', '--json']); }
        catch (error) { if (this.enabled) Main.notifyError('LocalSend', error.message); }
        finally { this.starting = false; if (this.enabled) this.schedule(10); }
    }
    async stop() {
        if (this.stopping || !this.transfer.motion.online) return; this.stopping = true;
        try { await this.run([this.binary, 'stop', '--force']); }
        catch (error) { if (this.enabled) Main.notifyError('LocalSend', error.message); }
        finally { this.stopping = false; if (this.enabled) this.schedule(10); }
    }
    chooseFiles() {
        if (this.picker) return;
        try {
            const helper = GLib.build_filenamev([this.path, 'send_dialog.py']);
            this.picker = Gio.Subprocess.new(['/usr/bin/python3', helper, '--binary', this.binary], Gio.SubprocessFlags.NONE);
            this.picker.wait_async(null, (source, result) => {
                try { source.wait_finish(result); } catch (_) { /* Selector was closed. */ } this.picker = null;
            });
        } catch (error) { Main.notifyError('LocalSend', error.message); }
    }
    disable() {
        this.enabled = false; this.generation++; removeSource(this.timer); this.timer = 0;
        this.monitor?.cancel(); this.monitor = null;
        for (const request of this.requests) request.cancel();
        if (this.panelHeightSignal) Main.panel.disconnect(this.panelHeightSignal); this.panelHeightSignal = 0;
        this.transfer?.destroy(); this.transfer = null; this.picker?.force_exit(); this.picker = null;
    }
}
