import {Motion, ENTER_MS, LEAVE_MS} from './localsend-transfer@local/motion.mjs';
import {view} from './localsend-transfer@local/model.mjs';
let cases = 0;
function check(ok, message) { if (!ok) throw new Error(message); cases++; }
const state = (overrides = {}) => ({...view({id: 'one', alias: '手机', phase: 'running',
    bytes: 20, total_bytes: 100, paths: ['/tmp/file'], remaining_ms: null, ...overrides}, 'receive'),
    direction: 'receive'});
const motion = new Motion();
check(motion.update(state(), 0), 'first snapshot starts entrance');
check(motion.sample(0).reveal === 0, 'entrance starts hidden');
check(motion.sample(300).reveal === 0.5, 'soft entrance midpoint');
check(!motion.update(state({bytes: 80}), 300), 'progress polling is the same transfer');
check(motion.started === 0, 'polling never restarts entrance');
check(motion.sample(340).fraction > 0.2 && motion.sample(340).fraction < 0.8, 'interpolation stays between observed samples');
check(motion.sample(600).reveal === 1 && motion.sample(600).fraction === 0.8, 'entrance and progress finish independently');
motion.update(state({phase: 'success', remaining_ms: 5000}), 700);
check(motion.sample(700).fraction === 1, 'success immediately uses final real progress');
check(motion.sample(700).resultReveal === 0, 'result starts its own entrance');
check(motion.sample(1000).resultReveal === 0.5, 'result always uses 600 ms');
check(motion.sample(1300).resultReveal === 1, 'result entrance complete');
motion.update(state({phase: 'success', remaining_ms: 5000}), 1200);
check(motion.deadline === 5700, 'repeated stale result cannot extend lifetime');
check(motion.sample(5300).reveal === 1, 'fade is inside the last 400 ms');
check(motion.sample(5500).reveal === 0.5, 'exit retracts along same edge');
check(motion.sample(5700).expired && motion.sample(5700).next === null, 'expires at backend deadline, no late timer');
for (const duration of [0, 150, 450, 5600]) {
    const m = new Motion();
    m.update(state(), 0);
    m.update(state({phase: 'success', remaining_ms: 5000}), duration);
    check(m.started === 0, `completion at ${duration} does not restart entrance`);
    check(m.sample(duration + ENTER_MS / 2).resultReveal === 0.5, `same result fade at ${duration}`);
    check(m.deadline === duration + 5000, `five seconds from completion at ${duration}`);
}
const direct = new Motion();
direct.update(state({phase: 'success', remaining_ms: 3800}), 1000);
check(direct.sample(1000).fraction === 1 && direct.sample(1000).reveal === 0, 'first snapshot may already be completed');
check(direct.deadline === 4800, 'late first snapshot does not restart five-second hold');
direct.update(state({id: 'two', bytes: 5}), 4600);
check(!direct.sample(4800).expired && direct.sample(4800).fraction <= 0.05, 'old expiry cannot hide or fill new transfer');
check(direct.update({...state({id: 'two'}), direction: 'send'}, 4800), 'direction participates in identity');
const pending = new Motion();
pending.update(state({phase: 'pending', bytes: 0}), 0);
check(!pending.sample(6000).expired && pending.sample(6000).next === null, 'pending timeout stays owned by backend');
pending.update(state({phase: 'running', bytes: 10}), 6000);
check(pending.started === 0, 'accepting does not restart the whole row');
pending.update(state({phase: 'failed', bytes: 37, remaining_ms: 5000}), 6100);
check(pending.sample(6100).fraction === 0.37 && pending.sample(6100).shine === null, 'failure preserves bytes and stops shimmer');
pending.update(state({phase: 'cancelled', bytes: 37, remaining_ms: 4800}), 6300);
check(pending.result === 'cancelled', 'cancellation has distinct result');
const reduced = new Motion();
reduced.update(state({phase: 'success', remaining_ms: 5000}), 0);
check(reduced.sample(0, false).reveal === 1 && reduced.sample(0, false).resultReveal === 1, 'system animation switch removes all entrance motion');
check(reduced.sample(0, false).next === 5000 && reduced.sample(5000, false).expired, 'reduced motion keeps the same deadline without frame loop');
check(ENTER_MS === 600 && LEAVE_MS === 400, 'approved timing constants');
print(`PASS: ${cases} motion, byte interpolation, identity, expiry and reduced-motion checks`);
