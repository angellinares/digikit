// The records a module committed, read from the emulated device (docs/plans/sdk.md 6.4). The runtime's `MODULE_COMMIT` log event names the
// RAM address and size of the module's committed record block, in the format of the register table. The block is decoded by
// `patchkit records --json` (one decoder, `register::Register::parse`), never here.
//
// `module:check` takes `[uses]` and the gallery param names from the output, and conformance its cases (slot, min, max, default).
// The CLI that boots a device and calls this is tools/devtools/commit_records.ts.
/** The part of a log event the decoder reads (`DkLogEvent` and `RuntimeEvent` of @digi/dk-session have it). */
export interface LogEvent {
  args: number[];
}

/** What the `MODULE_COMMIT` event says of one committed module. */
export interface CommitEvent {
  /** The module's index in the selection (the `index` of MODULES and MOD_LOAD). */
  index: number;
  machineRows: number;
  paramRows: number;
  pageRows: number;
  /** RAM address and byte size of the committed record block. */
  addr: number;
  size: number;
}

/**
 * The one place that knows how `MODULE_COMMIT` (log kind 44, `sdk/sdk.toml`) packs its fields into the five log arguments:
 * [index, rows, records_addr, records_size, 0] with rows = machine_rows | param_rows << 8 | page_rows << 16. `index` is the selection index
 * (0xffffffff: not a boot load).
 */
export function decodeCommitEvent(event: LogEvent): CommitEvent {
  const [index, rows, addr, size] = event.args as [number, number, number, number, number];
  return {
    index,
    machineRows: rows & 0xff,
    paramRows: (rows >>> 8) & 0xff,
    pageRows: (rows >>> 16) & 0xff,
    addr,
    size,
  };
}

/** `size` bytes of guest memory at `addr` (a multiple of 4), from longword reads, big-endian as the guest holds them. */
export async function readBlock(
  peek: (addr: number) => Promise<number | null>,
  addr: number,
  size: number,
): Promise<Uint8Array> {
  if (addr % 4 !== 0 || size % 4 !== 0) throw new Error(`record block ${addr.toString(16)}+${size}: not word aligned`);
  const out = Buffer.alloc(size);
  for (let at = 0; at < size; at += 4) {
    const word = await peek(addr + at);
    if (word === null) throw new Error(`record block: address ${(addr + at).toString(16)} is unmapped`);
    out.writeUInt32BE(word >>> 0, at);
  }
  return out;
}

/** The decoded records (`patchkit records --json`): `counts` are the three `[uses]` numbers, then machines, params, pages and effects. */
export interface Records {
  counts: { machine_rows: number; param_rows: number; page_rows: number };
  device: number;
  machines: Record<string, unknown>[];
  params: {
    index: number;
    template: number;
    slot: number;
    min: number;
    max: number;
    default: number;
    name: string;
    short: string;
  }[];
  pages: Record<string, unknown>[];
  effects: Record<string, unknown>[];
}

/** Runs `patchkit records --json -` with BLOCK on stdin and returns stdout (the caller owns the patchkit spawn: runtime/patchkit.ts). */
export type RecordsExec = (args: string[], options: { input: Uint8Array }) => Promise<{ stdout: string }>;

/** Decode a record block with `patchkit records`. */
export async function decodeRecords(exec: RecordsExec, block: Uint8Array): Promise<Records> {
  const { stdout } = await exec(['records', '--json', '-'], { input: block });
  return JSON.parse(stdout) as Records;
}

/**
 * The records of one committed module: read the block the event names, decode it, and check that the event's counts agree with the
 * block's (a disagreement means the runtime or this decoder is wrong, so it fails instead of reporting either).
 */
export async function committedRecords(
  event: LogEvent,
  peek: (addr: number) => Promise<number | null>,
  exec: RecordsExec,
): Promise<{ commit: CommitEvent; records: Records }> {
  const commit = decodeCommitEvent(event);
  const records = await decodeRecords(exec, await readBlock(peek, commit.addr, commit.size));
  const got = [records.counts.machine_rows, records.counts.param_rows, records.counts.page_rows];
  const want = [commit.machineRows, commit.paramRows, commit.pageRows];
  if (got.some((n, i) => n !== want[i])) {
    throw new Error(
      `MODULE_COMMIT says rows ${want.join('/')} but the record block holds ${got.join('/')} (machine/param/page)`,
    );
  }
  return { commit, records };
}
