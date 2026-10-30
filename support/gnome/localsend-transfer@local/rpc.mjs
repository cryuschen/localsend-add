// Modified by localsend-add contributors, 2026. See NOTICE and LICENSE.
import Gio from 'gi://Gio';
import GLib from 'gi://GLib';

// Unix socket only; does not spawn the CLI or perform device discovery.
export async function rpc(path, request, cancellable) {
    const client = new Gio.SocketClient({timeout: 2});
    let connection;
    try {
        connection = await new Promise((resolve, reject) => {
            client.connect_async(new Gio.UnixSocketAddress({path}), cancellable, (source, result) => {
                try { resolve(source.connect_finish(result)); } catch (error) { reject(error); }
            });
        });
        const bytes = new TextEncoder().encode(`${JSON.stringify(request)}\n`);
        await new Promise((resolve, reject) => {
            connection.get_output_stream().write_all_async(bytes, GLib.PRIORITY_DEFAULT, cancellable,
                (source, result) => {
                    try { source.write_all_finish(result); resolve(); } catch (error) { reject(error); }
                });
        });
        const chunks = [];
        let length = 0;
        while (length < 256 * 1024) {
            const bytesIn = await new Promise((resolve, reject) => {
                connection.get_input_stream().read_bytes_async(16384, GLib.PRIORITY_DEFAULT, cancellable,
                    (source, result) => {
                        try { resolve(source.read_bytes_finish(result)); } catch (error) { reject(error); }
                    });
            });
            if (!bytesIn.get_size()) throw new Error('Empty LocalSend response');
            const chunk = bytesIn.toArray();
            chunks.push(chunk);
            length += chunk.length;
            if (length > 256 * 1024) throw new Error('LocalSend response too large');
            if (chunk.includes(10)) {
                const complete = new Uint8Array(length);
                let offset = 0;
                for (const part of chunks) { complete.set(part, offset); offset += part.length; }
                return JSON.parse(new TextDecoder().decode(complete.subarray(0, complete.indexOf(10))));
            }
        }
        throw new Error('LocalSend response too large');
    } finally {
        connection?.close(null);
    }
}
