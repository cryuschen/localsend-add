import {view, selectTransfer} from './localsend-transfer@local/model.mjs';
let cases = 0;
function assert(ok, message) { if (!ok) throw new Error(message); cases++; }
const base = {id: '1', alias: '手机', phase: 'running', bytes: 68, total_bytes: 100,
    remaining_ms: null, paths: ['/tmp/真实文件 (2).txt']};
assert(view(null, 'send') === null, 'idle hidden');
const active = view(base, 'receive');
assert(active.fraction === 0.68 && !active.isError && !active.canOpen, 'running blue, open disabled');
assert(!['left', 'percent', 'result'].some(key => key in active), 'no top-bar status strings');
const success = view({...base, phase: 'success', remaining_ms: 4999}, 'receive');
assert(success.fraction === 1 && success.canOpen && !success.isError, 'success full green, open enabled');
assert(!view({...base, phase: 'success', paths: []}, 'receive').canOpen, 'missing file cannot open');
assert(!view({...base, phase: 'success'}, 'send').canOpen, 'no sender open action');
assert(view({...base, phase: 'success'}, 'send').sentSuccessfully, 'sender result shows success text');
assert(!view({...base, phase: 'running', bytes: 100}, 'send').sentSuccessfully, 'full byte counter alone does not imply successful send');
assert(!success.sentSuccessfully, 'receive keeps only open action');
for (const phase of ['running', 'failed', 'cancelled', 'pending'])
    assert(!view({...base, phase}, 'send').sentSuccessfully, 'no success text for '+phase);
for (const direction of ['receive', 'send']) {
    for (const phase of ['failed', 'cancelled']) {
        const state = view({...base, phase}, direction);
        assert(state.isError && state.fraction === 0.68 && !state.canOpen, `${direction} ${phase} keeps red partial progress`);
        const zero = view({...base, phase, bytes: 0}, direction);
        assert(zero.isError && zero.fraction === 0, `${direction} ${phase} zero-progress error`);
    }
}
assert(view({...base, phase: 'pending'}, 'receive').phase === 'pending', 'decision state retained');
assert(view({...base, remaining_ms: 0}, 'receive') === null, 'expired hidden');
assert(view({...base, total_bytes: 0}, 'send').fraction === 0, 'zero bytes finite');
assert(view({...base, phase: 'success', total_bytes: 0}, 'receive').fraction === 1, 'empty file success');
assert(view({...base, bytes: 200}, 'send').fraction === 1, 'progress bounded');
assert(selectTransfer({receive:null,send:null}) === null, 'one idle slot hidden');
assert(selectTransfer({receive:base,send:null}).direction === 'receive', 'receive uses the slot');
assert(selectTransfer({receive:null,send:base}).direction === 'send', 'send reuses the slot');
const oldResult = {...base,id:'old',phase:'success',remaining_ms:1000};
assert(selectTransfer({receive:oldResult,send:base}).direction === 'send', 'current work replaces older result');
const latest = {...base,id:'new',phase:'failed',remaining_ms:4500};
assert(selectTransfer({receive:oldResult,send:latest}).id === 'new', 'latest result kept');
assert(selectTransfer({receive:{...oldResult,remaining_ms:0},send:null}) === null, 'expired result stays hidden');
print(`PASS: ${cases} panel state and single-slot cases`);
