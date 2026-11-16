// `@tauri-apps/api/core` in the live preview (live.vite.config.ts): `emu_call` reaches the running emulator's control socket through the dev
// server, the library folder and patchkit.wasm are read from the repository. The target selector sees no MIDI ports.
const post = async (method: string, params: unknown) => {
  const r = await fetch('/__emu', { method: 'POST', body: JSON.stringify({ method, params }) });
  const text = await r.text();
  if (!r.ok) throw new Error(text);
  return JSON.parse(text);
};
const bytes = async (url: string): Promise<ArrayBuffer> => {
  const r = await fetch(url);
  if (!r.ok) throw new Error(await r.text());
  return r.arrayBuffer();
};

export async function invoke<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  const device = encodeURIComponent(String(args?.device ?? ''));
  switch (cmd) {
    case 'emu_call':
      return (await post(String(args?.method), args?.params)) as T;
    case 'lib_scan':
      return (await (await fetch(`/__lib/scan?device=${device}`)).json()) as T;
    case 'lib_read':
      return (await bytes(`/__lib/read?device=${device}&path=${encodeURIComponent(String(args?.path))}`)) as T;
    case 'patchkit_wasm':
      return (await bytes('/__wasm/patchkit')) as T;
    case 'bind_bundle':
      return new TextEncoder().encode('{"ports":[],"fw":[]}').buffer as T;
    case 'midi_ports':
      return [] as T;
    case 'card_open_folder':
    case 'midi_close':
    case 'midi_cancel':
      return undefined as T;
    default:
      throw new Error(`live preview: no stand-in for the Tauri command ${cmd}`);
  }
}
