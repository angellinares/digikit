// The events of the runtime's log (LOG) in words: the kind and argument names from sdk/sdk.toml (gen_proto.ts), the readable forms of the
// arguments, the one-line text, and the text of a fault with the events that belong to it. The same rules as `tools/device/dklog.py`
// (`event_dict`, `text_of`, `crash_text`, `hang_text`), which tests/digikit/test_dklog.py checks; apps/desktop/ui/src/log-follow*.ts follows the log with it.
import { GUARD_KINDS, LOG_FLAGS, LOG_KIND, LOG_KINDS, LOG_LEVELS } from './gen_proto.ts';
import type { DkLogEvent } from './dkupload-core.ts';
import { faultRows, type Fault } from './fault.ts';

/** LOG time is in audio blocks of the device. */
export const BLOCKS_PER_SECOND = 1500;

export type FieldValue = string | number | boolean | null | (string | number)[];

export interface RuntimeEvent {
  seq: number;
  blocks: number;
  /** The device time in seconds. */
  t: number;
  kind: string;
  kind_id: number;
  error: boolean;
  level: string;
  /** 0 the runtime, n the module with record index n - 1. */
  source: number;
  args: number[];
  fields: Record<string, FieldValue>;
}

const CARD_STATES = ['unknown', 'ours', 'blank', 'foreign', 'unreadable', 'too small'];
const SAFE_REASONS: Record<number, string> = { 1: 'counter', 2: 'key', 3: 'flag', 4: 'dsp' };
const REFUSAL: Record<number, string> = {
  1: 'outside the store range',
  2: 'card read-only',
  3: 'claim failed',
  4: 'card too small',
};
const REGS = [...Array.from({ length: 8 }, (_, i) => `d${i}`), ...Array.from({ length: 8 }, (_, i) => `a${i}`)];
const VECTORS: Record<number, string> = {
  2: 'access error',
  3: 'address error',
  4: 'illegal instruction',
  5: 'divide by zero',
  8: 'privilege violation',
  9: 'trace',
  10: 'line A',
  11: 'line F',
  14: 'format error',
};
const SITES = ['unknown', 'voice', 'fx master', 'fx send'];
const FAULT_KINDS: Record<number, string> = { 1: 'cpu crash', 2: 'dsp stopped', 3: 'cpu panic' };
const CRUMB_STAGES = [
  'NONE',
  'HOOK_ENTERED',
  'TYPE_FOUND',
  'STOCK_PATH_TAKEN',
  'ROUTINE_CALLED',
  'ROUTINE_RETURNED',
  'ODD_COPY_DONE',
  'EPILOGUE_DONE',
  'C3_TYPE_MATCHED',
  'C3_FRAME_SAVED',
  'C3_CONTEXT_PINNED',
  'D_AFTER_MODIFY_I7',
  'D_AFTER_R_STORES',
  'D_AFTER_I0_STORE',
  'D_AFTER_I1_I5_STORES',
  'D_AFTER_M1_M3_STORES',
  'D_AFTER_B3_I13_STORES',
  'D_AFTER_M5_M7_STORES',
  'D_AFTER_M13_M15_STORES',
  'D_AFTER_L0_L3_STORES',
  'D_AFTER_SV_L4_LOAD',
  'D_AFTER_L4_L5_STORES',
  'D_AFTER_MODE1_READ',
];
// The 32 words of dl_diag (modules/core/adapters/dsp-link/README.md), in order.
const DIAG_NAMES = (
  'magic_count link_beat voice_beat M5 M6 M7 M13 M14 M15 I6 I7 B7 L7 MODE1 crumb dispatches returns guard_faults guard_last ' +
  'guard_p01 guard_p23 guard_p45 guard_p67 guard_disabled_type fx_last module_step fx_master_calls fx_send_calls fx_faults ' +
  'record_this record_before fx_disabled_row'
).split(' ');
const DIAG_DECIMAL = new Set([
  'link_beat',
  'voice_beat',
  'dispatches',
  'returns',
  'guard_disabled_type',
  'module_step',
  'fx_master_calls',
  'fx_send_calls',
]);
const FX_ROW_BYTES = 96;
const NONE = 0xffffffff;

const hex8 = (n: number) => (n >>> 0).toString(16).padStart(8, '0');
const hex4 = (n: number) => (n & 0xffff).toString(16).padStart(4, '0');
const signed = (n: number) => n | 0;

/** Up to 4 characters of a big-endian word, up to the first NUL (`?` for a byte that is not printable ASCII), for each word given. */
export function asciiOf(...words: number[]): string {
  let out = '';
  for (const w of words)
    for (let s = 24; s >= 0; s -= 8) {
      const c = (w >>> s) & 255;
      if (c === 0) return out;
      out += c >= 32 && c < 127 ? String.fromCharCode(c) : '?';
    }
  return out;
}

const coreStep = (n: number) => ({ 0: 'none', 1: 'ENTER', 4: 'CALL', 5: 'RET' })[n] ?? `STAGE${n}`;
const crumbText = (w: number) =>
  `${CRUMB_STAGES[w & 0xff] ?? `STAGE${w & 0xff}`} voice=${(w >>> 8) & 0xff} type=${w >>> 16}`;
const fxLast = (w: number) =>
  w === 0
    ? 'NONE'
    : `${coreStep(w & 0xff)} ${SITES[(w >>> 8) & 0xff] ?? '?'} row=${Math.floor((w >>> 16) / FX_ROW_BYTES)}`;

/** The argument words of an event by name, with the readable forms (names, signed results) the kind has. */
export function fieldsOf(kindId: number, a: number[]): Record<string, FieldValue> {
  const [name, names] = LOG_KINDS[kindId] ?? [`KIND${kindId}`, ['a0', 'a1', 'a2', 'a3', 'a4']];
  let f: Record<string, FieldValue> = {};
  names.forEach((n, i) => {
    if (n !== '-') f[n] = a[i] ?? 0;
  });
  switch (name) {
    case 'CARD_CHECK':
      f.state = CARD_STATES[a[0]!] ?? a[0]!;
      f.first_nonblank = a[1] === NONE ? null : a[1]!;
      f.hash = hex8(a[2]!);
      break;
    case 'MOD_LOAD':
    case 'MOD_LIST':
      f = Object.fromEntries(Object.entries(f).filter(([k]) => !k.startsWith('name')));
      f.name = asciiOf(a[2]!, a[3]!, a[4]!);
      if (name === 'MOD_LOAD') f.result = signed(a[1]!);
      break;
    case 'CRASH_MOD':
      f =
        a[0] === 0x100
          ? { frame_word: hex8(a[1]!), faults: a[2]!, name_tail: asciiOf(a[3]!) }
          : {
              module: a[0] === NONE ? null : a[0]!,
              slots_entered: a[1]! & 0xffff,
              boot_was_stable: a[1]! >>> 16 !== 0,
              name: asciiOf(a[2]!, a[3]!, a[4]!),
            };
      break;
    case 'MOD_DSP':
      f.result = signed(a[1]!);
      break;
    case 'SAFE':
      f.reason = SAFE_REASONS[a[0]!] ?? a[0]!;
      break;
    case 'UPLOAD':
      f.status = signed(a[1]!);
      f.cmd = `0x${a[0]!.toString(16)}`;
      break;
    case 'CARD_REFUSED':
      f.reason = REFUSAL[a[2]!] ?? a[2]!;
      f.card_state = CARD_STATES[a[3]!] ?? a[3]!;
      break;
    case 'MOD_LOG':
      f = { msg: asciiOf(a[0]!, a[1]!, a[2]!, a[3]!), value: a[4]! };
      break;
    case 'CRASH_FOUND': {
      const vec = a[1]! & 0xff;
      f = {
        pc: hex8(a[0]!),
        vector: vec,
        vector_name: VECTORS[vec] ?? '?',
        format: a[1]! >>> 8,
        sr: hex4(a[2]!),
        a7: hex8(a[3]!),
        crash_time_blocks: a[4]!,
      };
      break;
    }
    case 'HEALTH': {
      const flags = a[0]! >>> 16;
      f = {
        health_flags: flags,
        cpu_pct: flags & 2 ? (a[0]! & 0xffff) / 10 : null,
        dsp_avg_pct: flags & 4 ? a[1]! / 10 : null,
        dsp_max_pct: flags & 4 ? a[2]! / 10 : null,
        arena_free: a[3]!,
        heap_free_kb: a[4] === NONE ? null : a[4]!,
      };
      break;
    }
    case 'DSP_DIAG': {
      const count = a[0]! >>> 8;
      const chunk = a[0]! & 0xff;
      f = { count, chunk };
      for (let k = 0; k < 4; k++) {
        const j = chunk * 4 + k;
        const n = DIAG_NAMES[j];
        if (j >= count || n === undefined) break;
        const w = a[1 + k]!;
        f[n] = n === 'crumb' ? crumbText(w) : n === 'fx_last' ? fxLast(w) : DIAG_DECIMAL.has(n) ? w : hex8(w);
      }
      break;
    }
    case 'DSP_STOPPED':
      f = {
        heartbeat: a[0]!,
        voice_beat: a[1]!,
        crumb: a[2] ? crumbText(a[2]) : 'NONE',
        dispatches: a[3]!,
        resumed: !!a[4],
      };
      break;
    case 'MACHINE_CHANGE':
      f = { track: a[0]! + 1, old_type: a[1] === NONE ? null : a[1]!, new_type: a[2] === NONE ? null : a[2]! };
      break;
    case 'MACHINE_FAULT':
      f = {
        type: a[0]! & 0xffff,
        voice: a[0]! >>> 16,
        track: Math.floor((a[0]! >>> 16) / 2) + 1,
        kind: GUARD_KINDS[a[1]! & 0xff] ?? String(a[1]! & 0xff),
        faults: a[1]! >>> 8,
        params: a.slice(2).flatMap((w) => [hex4(w), hex4(w >>> 16)]),
      };
      break;
    case 'MACHINE_DISABLED':
      f = { type: a[0]!, disabled: a[1]!, faults: a[2]! };
      break;
    case 'FX_FAULT':
    case 'FX_DISABLED':
      f = { row: a[0]!, faults: a[1]! };
      break;
    case 'FAULT_FOUND':
      f = {
        kind: FAULT_KINDS[a[0]! & 0xff] ?? a[0]! & 0xff,
        site: SITES[(a[0]! >>> 8) & 0xff] ?? (a[0]! >>> 8) & 0xff,
        core_step: coreStep((a[0]! >>> 16) & 0xff),
        id: a[1]!,
        module_step: a[2]!,
        at: hex8(a[3]!),
        module: a[4] === NONE ? null : a[4]!,
      };
      break;
    case 'CRASH_REG':
      f = Object.fromEntries(a.slice(1).flatMap((w, i) => (a[0]! + i < 16 ? [[REGS[a[0]! + i]!, hex8(w)]] : [])));
      break;
    case 'CRASH_STACK':
      f = { first_word: a[0]!, words: a.slice(1).map(hex8) };
      break;
  }
  return f;
}

/** One LOG event with its flags decoded and its arguments named. */
export function runtimeEvent(ev: DkLogEvent): RuntimeEvent {
  const lv = (ev.flags >> LOG_FLAGS.LEVEL_SHIFT) & LOG_FLAGS.LEVEL_MASK;
  return {
    seq: ev.seq,
    blocks: ev.time,
    t: Math.round((ev.time / BLOCKS_PER_SECOND) * 1000) / 1000,
    kind: LOG_KINDS[ev.kind]?.[0] ?? `KIND${ev.kind}`,
    kind_id: ev.kind,
    error: (ev.flags & LOG_FLAGS.ERROR) !== 0,
    level: LOG_LEVELS[lv] ?? String(lv),
    source: (ev.flags >> LOG_FLAGS.SOURCE_SHIFT) & LOG_FLAGS.SOURCE_MASK,
    args: [...ev.args],
    fields: fieldsOf(ev.kind, ev.args),
  };
}

/** Module record index + 1 -> name, from MOD_LIST events. */
export function moduleNames(events: DkLogEvent[]): Map<number, string> {
  const out = new Map<number, string>();
  for (const e of events)
    if (e.kind === LOG_KIND.MOD_LIST) out.set(e.args[0]! + 1, asciiOf(e.args[2]!, e.args[3]!, e.args[4]!));
  return out;
}

/** The module of an event: its name when known, else `mod#N` (N the record index); '' for the runtime. */
export const moduleOf = (source: number, names: ReadonlyMap<number, string>): string =>
  source === 0 ? '' : (names.get(source) ?? `mod#${source - 1}`);

const fieldText = (v: FieldValue): string => (v === null ? '-' : Array.isArray(v) ? v.join(',') : String(v));
const pairs = (fields: Record<string, FieldValue>) => Object.entries(fields).map(([k, v]) => `${k}=${fieldText(v)}`);

/** The text of an event for a log row: the kind, the module when a module wrote it, then `name=value` pairs. A module's own message
 * (MOD_LOG) is the module, the message in quotes and the value in hex. */
export function eventText(d: RuntimeEvent, names: ReadonlyMap<number, string>): string {
  const who = moduleOf(d.source, names);
  if (d.kind === 'MOD_LOG') {
    return [who || 'runtime', `"${d.fields.msg}"`, `value=0x${hex8(d.args[4]!)}`].join('  ');
  }
  return [d.kind, who, ...pairs(d.fields)].filter(Boolean).join('  ');
}

/** The readable line of dklog (`text_of`): sequence, device time, level, module, `!` for an error, kind, pairs. */
export function eventLine(d: RuntimeEvent, names: ReadonlyMap<number, string> = new Map()): string {
  const who = moduleOf(d.source, names);
  const mark = d.error ? '!' : ' ';
  return `${String(d.seq).padStart(6)} ${d.t.toFixed(3).padStart(10)}s ${d.level.padEnd(5)} ${who.padEnd(10)}${mark}${d.kind.padEnd(13)} ${pairs(d.fields).join(' ')}`;
}

/** Device time as the detail of a row shows it: `12.410s`. */
export const deviceTime = (blocks: number): string => `${(blocks / BLOCKS_PER_SECOND).toFixed(3)}s`;

/** The detail of an event: sequence, device time, level, source and the five raw arguments in hex. */
export function eventDetail(d: RuntimeEvent): string {
  return [
    `seq ${d.seq}   device time ${deviceTime(d.blocks)}   level ${d.level}   source ${d.source}${d.source ? ` (module record ${d.source - 1})` : ' (runtime)'}`,
    `args ${d.args.map(hex8).join(' ')}`,
  ].join('\n');
}

/** The kinds that belong to a fault record and are folded into its entry instead of showing as rows. */
export const FAULT_DETAIL_KINDS: ReadonlySet<number> = new Set([
  LOG_KIND.CRASH_REG,
  LOG_KIND.CRASH_STACK,
  LOG_KIND.CRASH_TAIL,
]);

/** The events logged by the runtime when it saw the crash (CRASH_TAIL pairs: seq, time, kind << 16 | flags, arg0, arg1; then seq, arg2, arg3, arg4, 1). */
function tailEvents(events: DkLogEvent[]): DkLogEvent[] {
  const out: DkLogEvent[] = [];
  let pending: number[] | null = null;
  for (const e of events) {
    if (e.kind !== LOG_KIND.CRASH_TAIL) continue;
    if (!pending) pending = e.args;
    else {
      const p = pending;
      out.push({
        seq: p[0]!,
        time: p[1]!,
        kind: p[2]! >>> 16,
        flags: p[2]! & 0xffff,
        args: [p[3]!, p[4]!, e.args[1]!, e.args[2]!, e.args[3]!],
      });
      pending = null;
    }
  }
  return out;
}

/**
 * The parts of a record that follow its first event, up to the next event of the same kind. Other events lie between them: a sink gets the small ring
 * first (FAULT_FOUND, the settings write, BOOT_RECORD) and the long detail of the record (registers, stack, tail) after it (`crash::replay_detail`).
 */
function followers(events: DkLogEvent[], at: number, parts: ReadonlySet<number>): DkLogEvent[] {
  const out: DkLogEvent[] = [];
  for (const e of events.slice(at + 1)) {
    if (e.kind === events[at]!.kind) break;
    if (parts.has(e.kind)) out.push(e);
  }
  return out;
}

const hexWords = (words: number[]) => words.map(hex8).join(' ');

/**
 * The text that goes under a fault entry: the ABOUT rows, where the record is kept, what the log of the boot after the fault says about
 * it (the vector, the registers and the stack of a crash; the stack of a panic; the place of a stopped DSP), and the events before the stop.
 * `events` are the events the log held at the boot that found the fault.
 */
export function faultDetail(f: Fault, events: DkLogEvent[], names: ReadonlyMap<number, string> = new Map()): string {
  const lines = [...faultRows(f), '', 'kept on the card as dk/fault (survives power cycles)'];
  const want = (e: DkLogEvent) =>
    f.kind === 'cpu'
      ? e.kind === LOG_KIND.CRASH_FOUND && e.args[4] !== 0xbad
      : e.kind === LOG_KIND.FAULT_FOUND && (e.args[0]! & 0xff) === (f.kind === 'dsp' ? 2 : 3);
  const at = events.map(want).lastIndexOf(true);
  if (at >= 0) {
    const first = events[at]!;
    const rest = followers(
      events,
      at,
      new Set([LOG_KIND.CRASH_MOD, LOG_KIND.CRASH_REG, LOG_KIND.CRASH_STACK, LOG_KIND.CRASH_TAIL, LOG_KIND.DSP_DIAG]),
    );
    const a = first.args;
    lines.push('', `found at the boot, log event ${first.seq}`);
    if (f.kind === 'cpu') {
      const vec = a[1]! & 0xff;
      lines.push(
        `vector ${vec} (${VECTORS[vec] ?? '?'}), format ${a[1]! >>> 8}, sr ${hex4(a[2]!)}`,
        `pc ${hex8(a[0]!)}   a7 ${hex8(a[3]!)}   crashed ${(a[4]! / BLOCKS_PER_SECOND).toFixed(1)} s after the boot before started`,
      );
    } else if (f.kind === 'panic') {
      lines.push(`a7 ${hex8(a[1]!)}; the file and line are not kept, the stack holds the way to it`);
    } else {
      lines.push(`core step ${coreStep((a[0]! >>> 16) & 0xff)}, module step ${a[2]}, last word ${hex8(a[3]!)}`);
    }
    const mod = rest.find((e) => e.kind === LOG_KIND.CRASH_MOD && e.args[0] !== 0x100);
    // the crashed module is named by its CRASH_MOD event: use it for the lines below, but keep the names of the current boot's list
    const known = new Map(names);
    if (mod && mod.args[0] !== NONE && !known.has(mod.args[0]! + 1))
      known.set(mod.args[0]! + 1, asciiOf(mod.args[2]!, mod.args[3]!, mod.args[4]!));
    if (mod) {
      const m = fieldsOf(mod.kind, mod.args);
      lines.push(
        m.module === null
          ? `outside any module, '${m.name}'`
          : `in module '${m.name}' (index ${m.module}); slots entered mask 0x${Number(m.slots_entered).toString(16)}; the boot had ${m.boot_was_stable ? 'already' : 'not yet'} reached its stable period`,
      );
    }
    const regs = new Map<string, number>();
    for (const e of rest.filter((x) => x.kind === LOG_KIND.CRASH_REG))
      e.args.slice(1).forEach((w, j) => e.args[0]! + j < 16 && regs.set(REGS[e.args[0]! + j]!, w));
    if (regs.size)
      for (let i = 0; i < 16; i += 4)
        lines.push(
          REGS.slice(i, i + 4)
            .map((r) => `${r} ${hex8(regs.get(r) ?? 0)}`)
            .join('  '),
        );
    const stack = new Map<number, number>();
    for (const e of rest.filter((x) => x.kind === LOG_KIND.CRASH_STACK))
      e.args.slice(1).forEach((w, j) => stack.set(e.args[0]! + j, w));
    if (stack.size) {
      const words = Array.from({ length: Math.max(...stack.keys()) + 1 }, (_, i) => stack.get(i) ?? 0);
      lines.push('stack from a7:');
      for (let i = 0; i < words.length; i += 8)
        lines.push(`  +${(4 * i).toString(16).padStart(3, '0')}  ${hexWords(words.slice(i, i + 8))}`);
    }
    const tail = tailEvents(rest);
    if (tail.length) {
      lines.push('', `events before the ${f.kind === 'dsp' ? 'stop' : 'crash'} (oldest first):`);
      for (const e of tail) lines.push(`  ${eventLine(runtimeEvent(e), known)}`);
    }
  }
  return lines.join('\n');
}
