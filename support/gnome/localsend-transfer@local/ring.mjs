import {selectTransfer} from './model.mjs';

export const COLORS = {white: [0.953, 0.953, 0.961], green: [0.392, 0.851, 0.545],
    blue: [0.412, 0.667, 1], purple: [154 / 255, 70 / 255, 1], red: [0.949, 0.463, 0.498]}; // Receive: #9A46FF.
const clamp = value => Math.max(0, Math.min(1, value));
const ease = value => value * value * (3 - 2 * value);
const approach = elapsed => 0.88 * (1 - Math.exp(-Math.max(0, elapsed) / 1400));

// Monotonic animation clocks; wall time is used only to translate the daemon's
// cross-process lifecycle timestamps when a new record arrives.
export class Ring {
    constructor() {
        this.phase = 'off'; this.online = false; this.fraction = 0;
        this.color = 'green'; this.pending = null; this.current = null;
        this.lifecycleKey = ''; this.dismissed = null; this.deadline = null;
        this.transition = null; this.progress = null;
    }

    layers() {
        if (this.phase === 'off') return [{color: 'white', end: 1}];
        if (this.phase === 'starting') return [{color: 'white', end: 1}, {color: 'green', end: this.fraction}];
        if (this.phase === 'stopping') return [{color: 'white', end: 1},
            ...this.transition.layers.map(layer => ({...layer, end: Math.min(layer.end, 1 - this.fraction)}))];
        // No transferred bytes means no arc to recolor. A solid red state ring
        // still signals failure; the stored byte fraction stays exactly zero.
        if (this.phase === 'error' && this.fraction === 0) return [{color: 'red', end: 1}];
        return [{color: 'green', end: 1}, {color: this.color, end: this.fraction}];
    }

    beginLifecycle(phase, now, start = now) {
        const layers = this.layers();
        this.phase = phase; this.fraction = 0; this.current = null;
        this.deadline = null; this.progress = null; this.pending = null;
        this.transition = {start, ready: null, finish: null, layers};
    }

    completeLifecycle(now, ended = now) {
        const transition = this.transition;
        if (!transition || transition.ready !== null) return;
        transition.ready = Math.max(transition.start, ended);
        transition.value = approach(transition.ready - transition.start);
        transition.finish = Math.max(transition.start + 2000, transition.ready + 180);
    }

    ingest(snapshot, lifecycle, now, wall = Date.now()) {
        const online = Boolean(snapshot?.ok && snapshot.running !== false);
        this.online = online;
        if (lifecycle?.version === 1 && typeof lifecycle.id === 'string') {
            const key = `${lifecycle.id}:${lifecycle.phase}:${lifecycle.changed_ms}`;
            const age = wall - lifecycle.changed_ms;
            if (key !== this.lifecycleKey) {
                this.lifecycleKey = key;
                // Ignore previous boots, stale files, and unrelated process IDs.
                const recent = age >= -1000 && age < 15000;
                const matching = !online || lifecycle.pid === snapshot.pid;
                const fromWall = stamp => now - Math.max(0, wall - stamp);
                if (recent && matching) {
                    if (lifecycle.phase === 'starting' || lifecycle.phase === 'running' && online) {
                        if (this.phase === 'off' || this.phase === 'starting' || this.phase === 'stopping') {
                            if (this.phase !== 'starting') this.beginLifecycle('starting', now, fromWall(lifecycle.started_ms));
                            else this.transition.start = fromWall(lifecycle.started_ms);
                            if (lifecycle.ready_ms !== null && online)
                                this.completeLifecycle(now, fromWall(lifecycle.ready_ms));
                        }
                    } else if (lifecycle.phase === 'stopping' || lifecycle.phase === 'stopped') {
                        if (this.phase !== 'stopping' && (this.phase !== 'off' || recent && lifecycle.stopping_ms !== null))
                            this.beginLifecycle('stopping', now, fromWall(lifecycle.stopping_ms ?? lifecycle.changed_ms));
                        if (this.phase === 'stopping' && !online && lifecycle.ended_ms !== null)
                            this.completeLifecycle(now, fromWall(lifecycle.ended_ms));
                    } else if (lifecycle.phase === 'failed' && !online) {
                        this.phase = 'off'; this.fraction = 0; this.transition = null;
                    }
                }
            }
        }
        if (this.phase === 'starting' && online) this.completeLifecycle(now);
        if (this.phase === 'stopping' && !online) this.completeLifecycle(now);
        if (!online) {
            this.pending = null;
            if (this.phase !== 'starting' && this.phase !== 'stopping' && this.phase !== 'off') {
                this.beginLifecycle('stopping', now); this.completeLifecycle(now);
            }
            this.sample(now);
            return;
        }
        if (this.phase === 'off') { this.phase = 'idle'; this.fraction = 0; }
        const transfer = selectTransfer(snapshot);
        this.pending = transfer?.phase === 'pending' ? transfer : null;
        if (this.phase === 'stopping') return;
        if (transfer && transfer.phase !== 'pending') {
            const key = `${transfer.direction}:${transfer.id}`;
            if (key !== this.dismissed) {
                const fresh = key !== this.current?.key;
                const terminal = transfer.phase !== 'running';
                const changedPhase = fresh || transfer.phase !== this.current?.phase;
                this.sample(now);
                this.current = {...transfer, key};
                this.color = transfer.isError ? 'red' : transfer.direction === 'send' ? 'blue' : 'purple';
                if (terminal) {
                    this.phase = transfer.isError ? 'error' : 'success';
                    this.fraction = transfer.fraction; this.progress = null; this.transition = null;
                    const expiry = now + Math.max(0, transfer.remaining_ms ?? 5000);
                    this.deadline = changedPhase || this.deadline === null ? expiry : Math.min(this.deadline, expiry);
                } else {
                    const from = fresh ? 0 : Math.min(this.fraction, transfer.fraction);
                    const changed = fresh || this.progress?.to !== transfer.fraction;
                    this.phase = 'transfer'; this.deadline = null; this.transition = null;
                    if (changed) this.progress = {start: now, from, to: transfer.fraction};
                }
            }
        } else if ((this.phase === 'error' || this.phase === 'success') && now >= this.deadline) {
            this.restore(now);
        } else if (this.phase === 'transfer' && !this.pending) {
            this.restore(now);
        }
        this.sample(now);
    }

    restore(now) {
        if (!['error', 'success', 'transfer'].includes(this.phase)) return;
        if (this.current) this.dismissed = this.current.key;
        this.transition = {start: now, from: this.fraction || (this.phase === 'error' ? 1 : 0)};
        this.phase = 'restoring'; this.progress = null; this.deadline = null;
    }

    sample(now, animations = true) {
        if (this.phase === 'starting' || this.phase === 'stopping') {
            const t = this.transition;
            if (this.phase === 'starting' && t.ready === null && !this.online && now - t.start >= 15000) {
                this.phase = 'off'; this.fraction = 0; this.transition = null;
                return {phase: this.phase, fraction: 0, layers: this.layers(), next: null};
            }
            this.fraction = t.ready === null ? approach(now - t.start) : t.value + (1 - t.value)
                * ease(clamp((now - t.ready) / Math.max(1, t.finish - t.ready)));
            if (!animations) this.fraction = t.ready === null || now < t.finish ? 0 : 1;
            if (t.finish !== null && now >= t.finish) {
                this.phase = this.phase === 'starting' ? 'idle' : 'off';
                this.fraction = 0; this.transition = null;
            }
        } else if (this.phase === 'restoring') {
            const amount = animations ? clamp((now - this.transition.start) / 700) : 1;
            this.fraction = this.transition.from * (1 - ease(amount));
            if (amount >= 1) { this.phase = this.online ? 'idle' : 'off'; this.fraction = 0; this.transition = null; }
        } else if (this.phase === 'transfer' && this.progress) {
            const t = this.progress;
            const amount = animations ? clamp((now - t.start) / 160) : 1;
            this.fraction = Math.min(t.to, t.from + (t.to - t.from) * ease(amount));
        } else if (['error', 'success'].includes(this.phase) && now >= this.deadline) {
            this.restore(now);
        }
        let next = null;
        if (['starting', 'stopping', 'restoring'].includes(this.phase)
            || this.phase === 'transfer' && this.progress && now < this.progress.start + 160) next = 16;
        else if (['error', 'success'].includes(this.phase)) next = Math.max(1, this.deadline - now);
        return {phase: this.phase, fraction: this.fraction, layers: this.layers(), next};
    }
}
