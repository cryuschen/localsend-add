import {Ring} from './localsend-transfer@local/ring.mjs';
let cases = 0;
function check(ok, message) { if (!ok) throw new Error(message); cases++; }
const snapshot = transfer => ({ok: true, running: true, pid: 42, receive: transfer, send: null});
const t = (overrides = {}) => ({id: 'one', alias: '手机', phase: 'running', bytes: 40,
    total_bytes: 100, remaining_ms: null, ...overrides});
const ring = new Ring();
check(ring.sample(0).layers[0].color === 'white', 'offline is a persistent white circle');
ring.beginLifecycle('starting', 0);
ring.ingest(snapshot(null), null, 800);
check(ring.sample(1500).phase === 'starting', 'fast start still lasts at least two seconds');
check(ring.sample(2000).phase === 'idle', 'two-second start finishes green');
check(ring.sample(2100).next === null, 'idle has no frame loop');
ring.ingest(snapshot(t({phase:'pending'})), null, 2200);
check(ring.pending && ring.phase === 'idle' && ring.fraction === 0, 'pending keeps the circle green');
ring.ingest(snapshot(null), null, 2300);
check(!ring.pending && ring.phase === 'idle', 'decline leaves the circle unchanged');
ring.ingest(snapshot(t()), null, 2400);
check(ring.sample(2480).fraction > 0 && ring.sample(2480).fraction < 0.4, 'real bytes are smoothly interpolated');
check(ring.sample(2560).fraction === 0.4 && ring.color === 'purple', 'receive is purple and never exceeds bytes');
ring.ingest(snapshot(t({phase:'cancelled',bytes:43,remaining_ms:5000})), null, 2600);
check(ring.phase === 'error' && ring.fraction === 0.43 && ring.color === 'red', 'cancel freezes bytes and turns red');
ring.ingest(snapshot(t({phase:'cancelled',bytes:43,remaining_ms:5000})), null, 3000);
check(ring.deadline === 7600, 'polling cannot extend the five-second result');
ring.restore(3100);
check(ring.sample(3450).fraction < 0.43 && ring.sample(3450).fraction > 0, 'red click retracts counterclockwise');
ring.ingest(snapshot(t({phase:'cancelled',bytes:43,remaining_ms:4000})), null, 3500);
check(ring.phase === 'restoring', 'dismissed result never reappears on a poll');
check(ring.sample(3800).phase === 'idle', 'dismiss restores green with backend alive');
for (const direction of ['send','receive']) {
    const r = new Ring();
    const s = transfer => ({ok:true,running:true,pid:42,[direction]:transfer});
    r.ingest(s(t({id:direction,phase:'success',remaining_ms:5000})), null, 0);
    check(r.phase === 'success' && r.fraction === 0, `${direction} unseen small success starts from green`);
    check(r.color === (direction==='send'?'blue':'purple'), `${direction} success retains its direction color`);
    check(r.sample(0).next === 16, 'successful small transfer schedules animation frames');
    check(r.sample(300).fraction === .5, 'small success sweeps clockwise instead of jumping full');
    r.ingest(s(t({id:direction,phase:'success',remaining_ms:4700})), null, 300);
    check(r.fraction === .5 && r.deadline === 5000, 'repeated success poll preserves the sweep and original deadline');
    r.ingest(s(null), null, 400);
    check(r.phase === 'success' && r.fraction > .5 && r.fraction < 1, 'result animation continues without another transfer snapshot');
    check(r.sample(600).fraction === 1 && r.sample(600).next === 4400, 'success fills in 600 ms then sleeps until expiry');
    r.ingest(s(t({id:direction,phase:'success',remaining_ms:4300})), null, 700);
    check(r.fraction === 1 && r.progress === null, 'completed sweep never restarts on polling');
    check(r.sample(4999).phase === 'success', 'full five-second hold');
    r.ingest(s(t({id:direction,phase:'success',remaining_ms:1})), null, 5000);
    check(r.phase === 'restoring', 'late success poll cannot resurrect an expired result');
    check(r.sample(5700).phase === 'idle', 'automatic restore ends green');
    r.ingest(s(t({id:'error',phase:'failed',bytes:20,remaining_ms:5000})), null, 6000);
    check(r.color === 'red' && r.fraction === .2, 'failure freezes the reported fraction');
    check(r.sample(11000).phase === 'restoring', 'failure automatically restores after five seconds');
    check(r.sample(11700).phase === 'idle', 'failure leaves the backend running');

    const partial = new Ring();
    partial.ingest(s(t({bytes:40})), null, 0);
    const visible = partial.sample(80).fraction;
    partial.ingest(s(t({phase:'success',remaining_ms:5000})), null, 80);
    check(partial.fraction === visible && visible === .2, 'success continues from the rendered partial arc');
    check(partial.sample(380).fraction > visible && partial.sample(380).fraction < 1, 'partial result smoothly completes the remaining arc');
    check(partial.sample(680).fraction === 1, 'partial success reaches full without resetting to zero');

    const full = new Ring();
    full.ingest(s(t({bytes:100})), null, 0); full.sample(160);
    full.ingest(s(t({phase:'success',remaining_ms:5000})), null, 200);
    check(full.fraction === 1 && full.progress === null, 'already rendered full progress stays full on success');

    const reducedSuccess = new Ring();
    reducedSuccess.ingest(s(t({phase:'success',remaining_ms:5000})), null, 0);
    check(reducedSuccess.sample(100,false).fraction === 1, 'reduced motion shows successful small transfers immediately');
    reducedSuccess.ingest(s(t({phase:'success',remaining_ms:4800})), null, 200);
    check(reducedSuccess.fraction === 1 && reducedSuccess.sample(200,false).next === 4800,
        'polling never rewinds reduced-motion success or starts a frame loop');

    const expiring = new Ring();
    expiring.ingest(s(t({phase:'success',remaining_ms:200})), null, 0);
    const shortArc = expiring.sample(100).fraction;
    expiring.sample(200);
    check(expiring.phase === 'restoring' && expiring.fraction >= shortArc && expiring.fraction < 1,
        'nearly expired result starts recovery on time even during completion animation');
    check(expiring.sample(900).phase === 'idle', 'short-lived success still recovers normally');

    const replaced = new Ring();
    replaced.ingest(s(t({phase:'success',remaining_ms:5000})), null, 0); replaced.sample(200);
    replaced.ingest(s(t({id:'next',bytes:30})), null, 200);
    check(replaced.phase === 'transfer' && replaced.fraction === 0, 'new transfer replaces the finishing success animation');
    check(replaced.sample(360).fraction === .3, 'new transfer still follows its own actual bytes');

    const stopped = new Ring();
    stopped.ingest(s(t({phase:'success',remaining_ms:5000})), null, 0); stopped.sample(300);
    stopped.beginLifecycle('stopping',300); stopped.ingest(null,null,400);
    check(stopped.phase === 'stopping' && stopped.sample(400).layers.at(-1).end === .5,
        'shutdown preserves the visible success arc and clears its animation');
    check(stopped.sample(2300).phase === 'off', 'shutdown during completion still ends white');
}
const slow = new Ring();
slow.beginLifecycle('starting', 0);
check(slow.sample(3000).phase === 'starting', 'slow backend is not falsely marked ready after two seconds');
slow.ingest(snapshot(null), null, 4000);
check(slow.sample(4180).phase === 'idle', 'slow start completes only after real ready signal');
slow.ingest(snapshot(t()), null, 4500); slow.sample(4660);
slow.beginLifecycle('stopping', 4700);
check(slow.sample(4800).layers[0].color === 'white', 'shutdown reveals white under the current colors');
slow.ingest(null, null, 5000);
check(slow.sample(6699).phase === 'stopping', 'fast shutdown still has a two-second minimum');
check(slow.sample(6700).phase === 'off', 'shutdown ends in persistent white');
const external = new Ring();
external.ingest(snapshot(null), {version:1,id:'external',pid:42,phase:'running',started_ms:10000,
    changed_ms:10800,ready_ms:10800}, 1000, 11000);
check(external.phase === 'starting', 'a fast external start is reconstructed from shared timestamps');
check(external.sample(2000).phase === 'idle', 'external start uses the original timeline');
external.ingest(null, {version:1,id:'external',pid:42,phase:'stopped',started_ms:10000,
    changed_ms:15100,ready_ms:10800,stopping_ms:15000,ended_ms:15100}, 5200, 15200);
check(external.phase === 'stopping', 'external stop synchronizes from its completion record');
check(external.sample(7000).phase === 'off', 'external shutdown uses actual stop-start time');
const stale = new Ring();
stale.beginLifecycle('starting',0);
check(stale.sample(15000).phase === 'off', 'failed startup cannot animate forever');
const reduced = new Ring();
reduced.ingest(snapshot(t({phase:'failed',remaining_ms:5000})), null, 0);
reduced.restore(100);
check(reduced.sample(100,false).phase === 'idle', 'system reduced motion skips recovery animation');
check(reduced.sample(100,false).next === null, 'reduced motion leaves no frame timer');
const zero = new Ring();
zero.ingest(snapshot(t({phase:'failed',bytes:0,remaining_ms:5000})),null,0);
check(zero.fraction === 0 && zero.layers()[0].color === 'red', 'zero-byte failure remains visibly red without inventing bytes');
zero.restore(100);
check(zero.sample(450).layers.at(-1).end > 0 && zero.sample(450).layers.at(-1).end < 1, 'zero-byte error can also retract counterclockwise');
check(zero.sample(800).phase === 'idle', 'zero-byte error restores green');
const killed = new Ring();
killed.ingest(null,{version:1,id:'killed',pid:42,phase:'running',started_ms:10000,changed_ms:11000,ready_ms:11000},0,12000);
check(killed.phase === 'off', 'orphaned running marker does not restart a dead backend animation');
print(`PASS: ${cases} ring lifecycle, transfer, deadline, cancellation and reduced-motion checks`);
