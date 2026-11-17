// Names for the addresses of the runtime log (docs/ADDRMAP.md "Workbench"): the resolver the Log pane gives to the text builders of
// packages/dk-session/src/runtime-log.ts, and the load table read from the memory of the running image through PEEK of the debug module
// (the emulator and a device alike). No DOM, no Tauri: `prof` is dkprof.wasm (packages/dk-session/src/dkprof.ts) and the session is a
// `DkSession` or a fake, so a test gives the real wasm and a fake memory.
// Host tests: packages/dk-session/tests/log-names.test.ts.
import type { AddrMapJson, DkProf, LoadTableJson } from './dkprof.ts';
import type { AddrNamer } from './runtime-log.ts';
import { CAP } from './gen_proto.ts';

/** What the controller answers to `runtime.maps` (docs/CONTROL.md). */
export interface RuntimeMaps {
  image_sha256: string;
  /** The maps belong to the running image's bytes; false: they may name the wrong code. */
  matches: boolean;
  /** The ship map, as JSON text. */
  ship: string;
  /** Byte addresses of the symbols the load table is read from. */
  symbols: Record<string, number>;
}

/** The part of dkprof the names need. */
export type NameProf = Pick<DkProf, 'addrLookup' | 'loadTable'>;

/**
 * The resolver of one image: `maps` (the ship map and the maps of the built modules) and the load table that says where the modules sit.
 * A module that is not in the table has no names, and an address no map holds has none: the answer is null and the caller keeps the hex.
 */
export function makeResolver(prof: NameProf, maps: AddrMapJson[], loads: LoadTableJson): AddrNamer {
  return (cpu, addr, ret = false) => {
    try {
      const [r] = prof.addrLookup({ maps, loads, ret, addrs: [{ cpu, addr }] }).results;
      if (!r?.text) return null;
      return r.module ? `${r.module}: ${r.text}` : r.text;
    } catch {
      return null;
    }
  };
}

/** Read `len` bytes of RAM at a byte address: PEEK of the debug module over a `DkSession`, for the emulator and for a device alike. */
export type Peek = (addr: number, len: number) => Promise<Uint8Array>;

/**
 * The load table of a running image: dkprof says which symbols to read and how many bytes (`reads`, from the loader header of the ship
 * map), the bytes come from `peek` at the symbol addresses of the image, and dkprof decodes them.
 */
export async function readLoadTable(
  prof: NameProf,
  ship: AddrMapJson,
  symbols: Record<string, number>,
  peek: Peek,
): Promise<LoadTableJson> {
  const { reads } = prof.loadTable({ maps: [ship] }) as { reads: { symbol: string; len: number }[] };
  const snapshot: Record<string, string> = {};
  for (const { symbol, len } of reads) {
    const base = symbols[symbol];
    if (base === undefined) throw new Error(`the image has no symbol ${symbol}`);
    snapshot[symbol] = Array.from(await peek(base, len), (b) => b.toString(16).padStart(2, '0')).join('');
  }
  return prof.loadTable({ maps: [ship], snapshot }) as LoadTableJson;
}

/** The ship map of the image a session talks to, with the addresses of its load-table symbols. */
export interface ShipSource {
  /** The ship map as JSON text. */
  ship: string;
  symbols: Record<string, number>;
  /** The map belongs to the bytes that run (the controller says so for the emulator). Default: yes. */
  matches?: boolean;
}

/** The part of a protocol session the names read: INFO (capabilities, the image id) and PEEK. */
export interface NameSession {
  info(quick?: boolean): Promise<{ caps: number | null; image_id?: string | null; port?: string }>;
  peek?(addr: number, len: number): Promise<Uint8Array>;
}

/** What a target supplies: where its ship map comes from, and the maps of the built modules. */
export interface NameSources {
  prof(): Promise<NameProf>;
  /** The ship map of the image behind the session, or why there is none (a short reason). */
  ship(session: NameSession): Promise<ShipSource | { unavailable: string }>;
  /** The JSON texts of the module maps of a device (`dt2`, `dn2`). */
  moduleMaps(device: string): Promise<string[]>;
}

/** The names of one origin the follower reads: `resolve` is what the text builders take, `refresh` reads the maps and the load table again. */
export interface LogNames {
  /** The resolver of the last refresh, or undefined when there is none. */
  resolve(): AddrNamer | undefined;
  /** Why there are no names (a short reason), after a refresh that made none; undefined while there are. */
  unavailable(): string | undefined;
  /** The maps and the load table behind `resolve` (the Profile pane names DSP addresses with them), or undefined when there are none. */
  inputs(): { maps: AddrMapJson[]; loads: LoadTableJson } | undefined;
  refresh(session: NameSession): Promise<void>;
}

const PEEK_CAP = 'needs the debug module (digikit/debug): its PEEK reads the load table';

/** The names of one target. The same code serves the emulator and a device: they differ in `sources.ship` only. */
export function createLogNames(sources: NameSources): LogNames {
  let resolver: AddrNamer | undefined;
  let inputs: { maps: AddrMapJson[]; loads: LoadTableJson } | undefined;
  let why: string | undefined;
  const none = (reason: string) => {
    resolver = undefined;
    inputs = undefined;
    why = reason;
  };
  return {
    resolve: () => resolver,
    unavailable: () => why,
    inputs: () => inputs,
    async refresh(session) {
      resolver = undefined;
      const info = await session.info();
      if (info.caps === null || ((info.caps >>> CAP.PEEK) & 1) === 0 || !session.peek) return none(PEEK_CAP);
      const src = await sources.ship(session);
      if ('unavailable' in src) return none(src.unavailable);
      if (src.matches === false)
        return none('the maps are not those of the image that runs (reload or flash it again)');
      const ship = JSON.parse(src.ship) as AddrMapJson;
      const device = (ship.key as { device?: string } | undefined)?.device;
      const modules = device ? await sources.moduleMaps(device) : [];
      const prof = await sources.prof();
      const loads = await readLoadTable(prof, ship, src.symbols, session.peek.bind(session));
      const maps = [ship, ...modules.map((m) => JSON.parse(m) as AddrMapJson)];
      resolver = makeResolver(prof, maps, loads);
      inputs = { maps, loads };
      why = undefined;
    },
  };
}
