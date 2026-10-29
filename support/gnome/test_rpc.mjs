import Gio from 'gi://Gio';
import {rpc} from './localsend-transfer@local/rpc.mjs';
const value = await rpc(ARGV[0], {command: 'panel'}, new Gio.Cancellable());
if (!value.ok) throw new Error(JSON.stringify(value));
print(JSON.stringify(value));
