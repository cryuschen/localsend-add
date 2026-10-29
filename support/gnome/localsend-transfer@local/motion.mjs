// Presentation clock only. Byte counts, outcomes and expiry belong to the CLI.
export const ENTER_MS = 600;
export const LEAVE_MS = 400;
const PROGRESS_MS = 160;
const clamp = value => Math.max(0, Math.min(1, value));
const smooth = value => { const t = clamp(value); return t * t * (3 - 2 * t); };

export class Motion {
    update(state, now) {
        const key = JSON.stringify([state.direction, state.id]);
        const fresh = key !== this.key;
        const previous = fresh ? 0 : this.sample(now).fraction;
        const result = state.canOpen ? 'open' : state.sentSuccessfully ? 'success'
            : state.isError ? state.phase : '';
        if (fresh) {
            this.key = key;
            this.started = now;
            this.result = '';
            this.deadline = null;
            this.phase = null;
        }
        if (result !== this.result) this.resultStarted = now;
        if (state.phase !== this.phase) this.phaseStarted = now;
        this.result = result;
        this.phase = state.phase;
        this.state = state;
        if (Number.isFinite(state.remaining_ms)) {
            const deadline = now + Math.max(0, state.remaining_ms);
            this.deadline = this.deadline === null ? deadline : Math.min(this.deadline, deadline);
        } else {
            this.deadline = null;
        }
        if (fresh || state.fraction !== this.target) {
            this.from = Math.min(previous, state.fraction);
            this.target = state.fraction;
            this.progressStarted = now;
        }
        return fresh;
    }

    sample(now, animations = true) {
        const state = this.state;
        const remaining = this.deadline === null ? Infinity : this.deadline - now;
        const leaving = remaining < LEAVE_MS;
        const expired = remaining <= 0;
        const entrance = animations ? smooth((now - this.started) / ENTER_MS) : 1;
        const reveal = expired ? 0 : Math.min(entrance,
            animations ? smooth(remaining / LEAVE_MS) : 1);
        const resultReveal = !this.result ? 0 : animations
            ? smooth((now - this.resultStarted) / ENTER_MS) : 1;
        const terminal = state.phase !== 'running' && state.phase !== 'pending';
        const t = clamp((now - this.progressStarted) / PROGRESS_MS);
        const fraction = !animations || terminal ? this.target
            : Math.min(this.target, this.from + (this.target - this.from) * (1 - (1 - t) ** 3));
        const colorMix = !animations || state.isError ? 1 : smooth((now - this.phaseStarted) / 240);
        const pulseAge = now - this.phaseStarted;
        const pulse = animations && state.phase === 'success' && pulseAge < 440
            ? Math.sin(Math.PI * clamp(pulseAge / 440)) * 0.16 : 0;
        const moving = animations && (now - this.started < ENTER_MS
            || (this.result && now - this.resultStarted < ENTER_MS)
            || (state.phase === 'running' && fraction > 0)
            || (!terminal && t < 1 && this.from !== this.target)
            || pulse > 0 || (state.phase === 'success' && pulseAge < 440) || leaving);
        // Sleep through the static result hold; no permanent frame loop at idle.
        const next = expired ? null : moving ? 33
            : Number.isFinite(remaining) ? Math.max(1, remaining - (animations ? LEAVE_MS : 0)) : null;
        return {reveal, resultReveal, fraction, colorMix, pulse, leaving, expired, next,
            shine: animations && state.phase === 'running' && fraction > 0
                ? ((now - this.started) % 2000) / 2000 : null};
    }
}
