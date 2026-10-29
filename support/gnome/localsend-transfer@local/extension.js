import Clutter from 'gi://Clutter';
import Gio from 'gi://Gio';
import GLib from 'gi://GLib';
import Pango from 'gi://Pango';
import St from 'gi://St';
import {Extension} from 'resource:///org/gnome/shell/extensions/extension.js';
import * as Main from 'resource:///org/gnome/shell/ui/main.js';
import * as PanelMenu from 'resource:///org/gnome/shell/ui/panelMenu.js';
import * as PopupMenu from 'resource:///org/gnome/shell/ui/popupMenu.js';
import {rpc} from './rpc.mjs';
import {view, selectTransfer} from './model.mjs';
import {Motion} from './motion.mjs';
import {Reveal} from './reveal.mjs';

const nowMs = () => GLib.get_monotonic_time() / 1000;

function label(text = '', style = '') {
    return new St.Label({text, style_class: style, y_align: Clutter.ActorAlign.CENTER});
}

function button(text, action) {
    const widget = new St.Button({label: text, style_class: 'ls-transfer-action',
        can_focus: true, y_align: Clutter.ActorAlign.CENTER});
    widget.connect('clicked', action);
    return widget;
}

class TransferPanel {
    constructor(extension) {
        this.extension = extension;
        this.motion = new Motion();
        // Child buttons own their click gestures; the outer actor has no gesture.
        this.indicator = new PanelMenu.Button(0, 'LocalSend 传输', true);
        this.indicator.add_style_class_name('ls-transfer-group');
        this.row = new St.BoxLayout({style_class: 'ls-transfer-row'});
        this.indicator.add_child(this.row);
        this.peerButton = button('', () => this.indicator.menu.toggle());
        this.peerButton.set_style('background-color: transparent; padding: 0;');
        this.peer = label('', 'ls-transfer-peer');
        this.peer.clutter_text.ellipsize = Pango.EllipsizeMode.NONE;
        this.peerButton.set_child(this.peer);
        // Draw in absolute coordinates: the fill always starts at x=0,
        // independent of BinLayout alignment or the current text direction.
        this.progress = new St.DrawingArea({style_class: 'ls-transfer-progress',
            y_align: Clutter.ActorAlign.CENTER});
        this.progress.connect('repaint', area => this.paintProgress(area));
        this.accept = button('接收', () => extension.answer(this.current.id, true));
        this.decline = button('拒绝', () => extension.answer(this.current.id, false));
        this.open = button('打开', () => this.openFile());
        this.sent = label('传输成功');
        this.open.x_align = this.sent.x_align = Clutter.ActorAlign.START;
        // Invisible actors retain their natural size. Both decision controls and
        // all four-character results reserve space before they become visible.
        this.resultBox = new St.Widget({layout_manager: new Clutter.BinLayout(),
            y_align: Clutter.ActorAlign.CENTER});
        this.resultBox.add_child(this.open);
        this.resultBox.add_child(this.sent);
        this.transferBox = new St.BoxLayout({style_class: 'ls-transfer-row'});
        this.transferBox.add_child(this.progress);
        this.transferBox.add_child(this.resultBox);
        this.decisionBox = new St.BoxLayout({style_class: 'ls-transfer-row'});
        this.decisionBox.add_child(this.accept);
        this.decisionBox.add_child(this.decline);
        this.decisionBox.x_align = this.transferBox.x_align = Clutter.ActorAlign.START;
        this.body = new St.Widget({layout_manager: new Clutter.BinLayout(),
            y_align: Clutter.ActorAlign.CENTER});
        this.body.add_child(this.decisionBox);
        this.body.add_child(this.transferBox);
        this.row.add_child(this.peerButton);
        this.row.add_child(this.body);
        this.reveal = new Reveal();
        this.resultReveal = new Reveal();
        this.row.add_effect_with_name('localsend-reveal', this.reveal);
        this.resultBox.add_effect_with_name('localsend-result-reveal', this.resultReveal);
        this.animationSettings = St.Settings.get();
        this.animationSignal = this.animationSettings.connect('notify::enable-animations', () => {
            if (this.current) this.tick();
        });
        this.indicator.setMenu(new PopupMenu.PopupMenu(this.peerButton, 0, St.Side.TOP));
        // setMenu() re-enables GNOME's outer press gesture, even with
        // dontCreateMenu. Only the device-name button should toggle details.
        this.indicator._clickGesture.set_enabled(false);
        Main.panel.addToStatusArea('localsend-transfer', this.indicator, -1, 'left');
        this.indicator.container.hide();
    }

    update(transfer) {
        this.direction = transfer?.direction;
        const state = view(transfer, this.direction);
        this.current = state;
        if (!state) {
            this.stopAnimation();
            this.motion = new Motion();
            this.frame = null;
            this.menuKey = null;
            this.indicator.menu.close();
            this.indicator.container.hide();
            this.extension.layout();
            return;
        }
        const fresh = this.motion.update(state, nowMs());
        if (fresh) this.indicator.menu.close();
        this.peer.text = state.alias;
        const pending = state.phase === 'pending';
        this.decisionBox.opacity = pending ? 255 : 0;
        this.transferBox.opacity = pending ? 0 : 255;
        // Four-character results retain the same natural reserved width.
        this.sent.text = state.isError ? state.phase === 'cancelled' ? '传输取消' : '传输失败' : '传输成功';
        this.open.opacity = state.canOpen ? 255 : 0;
        this.sent.opacity = state.sentSuccessfully || state.isError ? 255 : 0;
        this.tick();
        this.extension.layout();
        // Keep an open details menu stable as byte counters change.
        const key = JSON.stringify([this.direction, state.id, state.phase, state.paths, state.detail]);
        if (key !== this.menuKey) {
            this.menuKey = key;
            this.indicator.menu.removeAll();
            const addInfo = text => this.indicator.menu.addMenuItem(
                new PopupMenu.PopupMenuItem(text, {reactive: false, can_focus: false}));
            addInfo(`设备：${state.alias}`);
            if (this.direction === 'receive') addInfo(`保存位置：${state.destination}`);
            if (state.detail) addInfo(state.detail);
            if (state.file_count > 1 && state.canOpen) addInfo(`共 ${state.file_count} 个文件；顶栏“打开”打开第一个文件`);
            for (const path of (state.canOpen ? state.paths : [])) {
                const item = new PopupMenu.PopupMenuItem(GLib.path_get_basename(path));
                item.connect('activate', () => this.extension.launch(path));
                this.indicator.menu.addMenuItem(item);
            }
        }
    }

    openFile() {
        if (this.open.reactive && this.current?.canOpen) this.extension.launch(this.current.paths[0]);
    }

    stopAnimation() {
        if (this.animationTimer) GLib.Source.remove(this.animationTimer);
        this.animationTimer = 0;
    }

    fullyRevealed(actor, amount) {
        if (amount >= 1) return true;
        const [left] = this.row.get_transformed_position();
        const [width] = this.row.get_transformed_size();
        const [x] = actor.get_transformed_position();
        const [actorWidth] = actor.get_transformed_size();
        return width > 0 && actorWidth > 0 && (x + actorWidth - left) / width <= 1.24 * amount - 0.24;
    }

    tick() {
        this.stopAnimation();
        if (!this.current) return;
        const frame = this.motion.sample(nowMs(), this.animationSettings.enable_animations);
        this.frame = frame;
        this.indicator.container.visible = !frame.expired;
        if (frame.expired) this.indicator.menu.close();
        this.reveal.setProgress(frame.reveal);
        this.resultReveal.setProgress(frame.resultReveal);
        const pending = this.current.phase === 'pending';
        const interactive = actor => !frame.expired && this.fullyRevealed(actor, frame.reveal);
        this.peerButton.reactive = this.peerButton.can_focus = interactive(this.peerButton);
        for (const actor of [this.accept, this.decline])
            actor.reactive = actor.can_focus = pending && !this.extension.answering && interactive(actor);
        this.open.reactive = this.open.can_focus = Boolean(this.current.canOpen)
            && frame.resultReveal >= 1 && interactive(this.open);
        this.progress.queue_repaint();
        if (frame.next !== null) {
            this.animationTimer = GLib.timeout_add(GLib.PRIORITY_DEFAULT, frame.next, () => {
                this.animationTimer = 0;
                this.tick();
                return GLib.SOURCE_REMOVE;
            });
        }
    }

    paintProgress(area) {
        const [width, height] = area.get_surface_size();
        const context = area.get_context();
        const radius = Math.min(height / 2, width / 2);
        try {
            // Clip the trough and fill together to the same rounded outline.
            context.arc(width - radius, radius, radius, -Math.PI / 2, 0);
            context.arc(width - radius, height - radius, radius, 0, Math.PI / 2);
            context.arc(radius, height - radius, radius, Math.PI / 2, Math.PI);
            context.arc(radius, radius, radius, Math.PI, Math.PI * 1.5);
            context.closePath();
            context.clip();
            const state = this.current;
            const fraction = this.frame?.fraction ?? state?.fraction ?? 0;
            if (state?.isError && fraction === 0)
                context.setSourceRGBA(1, 0.54, 0.54, 0.35);
            else
                context.setSourceRGBA(1, 1, 1, 0.25);
            context.rectangle(0, 0, width, height);
            context.fill();
            const blue = [0.38, 0.63, 0.92];
            const target = state?.isError ? [1, 0.54, 0.54] : [0.56, 0.94, 0.64];
            const mix = state?.phase === 'success' || state?.isError ? this.frame?.colorMix ?? 1 : 0;
            const color = blue.map((value, i) => Math.min(1,
                value + (target[i] - value) * mix + (this.frame?.pulse ?? 0)));
            context.setSourceRGB(...color);
            context.rectangle(0, 0, width * fraction, height);
            context.fill();
            if (this.frame?.shine !== null && this.frame?.shine !== undefined && fraction > 0) {
                context.rectangle(0, 0, width * fraction, height);
                context.clip();
                const center = (this.frame.shine * 1.5 - 0.25) * width;
                const band = width * 0.22;
                // Small translucent strips form a soft highlight without a second texture.
                for (let i = 0; i < 16; i++) {
                    context.setSourceRGBA(1, 1, 1, 0.22 * Math.sin(Math.PI * i / 15));
                    context.rectangle(center - band / 2 + band * i / 16, 0, band / 16 + 0.2, height);
                    context.fill();
                }
            }
        } finally {
            context.$dispose();
        }
    }

    destroy() {
        this.stopAnimation();
        this.animationSettings.disconnect(this.animationSignal);
        this.indicator.destroy();
    }
}

export default class LocalSendTransfer extends Extension {
    enable() {
        this.enabled = true;
        this.requests = new Set();
        this.generation = (this.generation ?? 0) + 1;
        this.transfer = new TransferPanel(this);
        this.panelHeightSignal = Main.panel.connect('notify::height', () => this.layout());
        this.pathToSocket = GLib.build_filenamev([GLib.get_user_config_dir(), 'localsend-cli', 'control.sock']);
        this.poll();
        console.log('[localsend-transfer] enabled');
    }

    layout() {
        const container = this.transfer?.indicator.container;
        if (!container) return;
        const left = Main.panel._leftBox;
        const vitals = Main.panel.statusArea.vitalsMenu?.container;
        if (vitals?.get_parent() === left) {
            const children = left.get_children();
            if (children.indexOf(container) !== children.indexOf(vitals) + 1)
                left.set_child_above_sibling(container, vitals);
        }
        if (container.visible) {
            container.height = Main.panel.height;
            this.transfer.indicator.height = Main.panel.height;
        }
    }

    schedule(delay) {
        if (!this.enabled) return;
        if (this.timer) GLib.Source.remove(this.timer);
        this.timer = GLib.timeout_add(GLib.PRIORITY_DEFAULT, Math.max(10, delay), () => {
            this.timer = 0;
            this.poll();
            return GLib.SOURCE_REMOVE;
        });
    }

    async request(message) {
        const cancel = new Gio.Cancellable();
        this.requests.add(cancel);
        const timeout = GLib.timeout_add(GLib.PRIORITY_DEFAULT, 2200, () => {
            cancel.cancel();
            return GLib.SOURCE_REMOVE;
        });
        try { return await rpc(this.pathToSocket, message, cancel); }
        finally {
            // Removing an expired source logs an error; only remove a live one.
            if (GLib.MainContext.default().find_source_by_id(timeout)) GLib.Source.remove(timeout);
            this.requests.delete(cancel);
        }
    }

    render(snapshot) {
        const state = selectTransfer(snapshot);
        this.transfer.update(state);
        const expiry = state?.remaining_ms;
        // Completion expiry remains measured by the CLI, not by this poll.
        this.schedule(state ? Math.min(200, Number.isFinite(expiry) ? expiry : 200) : 1000);
    }

    async poll() {
        if (!this.enabled || this.polling) return;
        this.polling = true;
        const generation = this.generation ?? 0;
        try {
            const snapshot = await this.request({command: 'panel'});
            if (this.enabled && generation === (this.generation ?? 0)) this.render(snapshot);
        } catch (_) {
            if (this.enabled && generation === (this.generation ?? 0)) {
                this.transfer.update(null);
                this.schedule(1000);
            }
        } finally {
            this.polling = false;
            if (this.enabled && !this.timer) this.schedule(200);
        }
    }

    async answer(id, accept) {
        if (this.answering) return;
        this.answering = true;
        this.generation = (this.generation ?? 0) + 1;
        if (this.transfer.current) this.transfer.update(this.transfer.current);
        try {
            const snapshot = await this.request({command: 'answer_transfer', session_id: id, accept});
            if (this.enabled && snapshot.ok) this.render(snapshot);
        } catch (error) {
            console.warn(`[localsend-transfer] answer: ${error.message}`);
        } finally {
            this.answering = false;
            if (this.enabled) this.schedule(10);
        }
    }

    launch(path) {
        Gio.AppInfo.launch_default_for_uri_async(Gio.File.new_for_path(path).get_uri(),
            global.create_app_launch_context(0, -1), null, (_source, result) => {
                try { Gio.AppInfo.launch_default_for_uri_finish(result); }
                catch (error) { console.warn(`[localsend-transfer] open: ${error.message}`); }
            });
    }

    disable() {
        this.enabled = false;
        if (this.panelHeightSignal) Main.panel.disconnect(this.panelHeightSignal);
        this.panelHeightSignal = 0;
        if (this.timer) GLib.Source.remove(this.timer);
        this.timer = 0;
        for (const request of this.requests) request.cancel();
        this.transfer?.destroy();
        this.transfer = null;
        console.log('[localsend-transfer] disabled');
    }
}
