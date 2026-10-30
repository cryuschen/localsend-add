// Modified by localsend-add contributors, 2026. See NOTICE and LICENSE.
// Plain state projection, also exercised outside GNOME Shell.
export function view(transfer, direction) {
    if (!transfer || (transfer.remaining_ms !== null && transfer.remaining_ms <= 0))
        return null;
    const receiving = direction === 'receive';
    const phase = transfer.phase;
    const fraction = phase === 'success' ? 1 : transfer.total_bytes > 0
        ? Math.max(0, Math.min(1, transfer.bytes / transfer.total_bytes)) : 0;
    return {...transfer, fraction, isError: phase === 'failed' || phase === 'cancelled',
        sentSuccessfully: direction === 'send' && phase === 'success',
        canOpen: receiving && phase === 'success' && transfer.paths?.length > 0};
}


// The panel has one slot, reused for the current batch in either direction.
export function selectTransfer(snapshot) {
    const candidates = ['receive', 'send'].map(direction => {
        const state = view(snapshot[direction], direction);
        return state ? {...state, direction} : null;
    }).filter(Boolean);
    const active = candidates.find(state => state.phase === 'pending')
        ?? candidates.find(state => state.phase === 'running');
    if (active) return active;
    // A new batch result supersedes an older result still in its five seconds.
    return candidates.sort((a, b) => b.remaining_ms - a.remaining_ms)[0] ?? null;
}
