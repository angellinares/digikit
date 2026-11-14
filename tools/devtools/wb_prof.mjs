// The offline commands of wb (tools/devtools/wb.mjs): address names, module DSP analysis and profiles. Everything goes through
// apps/web/public/dkprof.wasm (packages/dkprof, loaded by packages/dk-session/src/dkprof.ts): the code the workbench uses. No child process.
//
//     wb [--format json|text] addrmap lookup ADDR... [--map F]... [--image X.syx] [--loads F | --snapshot F] [--load ID=BASE] [--dsp-base ID=PC] [--cpu cf|sharc] [--ret]
//     wb [--format json|text] module analyse MODULE.dkm DSP.dko.json
//     wb [--format json|text] profile report FILE [--top N] [--map F]... [--image X.syx] [--loads F] [--analyse ID=MODULE.dkm:DSP.dko.json]...
//     wb profile record --seconds N --out FILE [--image X.syx] [--run-dir DIR] [--since-boot] [--cpu cf|sharc]
//     wb [--port SUB] [--bin PATH] profile spans [--start|--stop|--reset] [--out FILE]   (a device command, run from wb.mjs: profSpans)
//
// `record` reads `cf.profile` before and after N seconds and keeps the difference; `--since-boot` keeps the cumulative counters (the state of a paused emulator).
// `--cpu sharc` records `sharc.profile` (the daemon needs the `sharc-profile` cargo feature): it starts the DSP counting, waits N seconds and reads it;
// `--since-boot` skips the start and the wait and reads what the counting has gathered since a `sharc.profile {enable: true}` (a paused emulator).
// `--analyse ID=MODULE.dkm:DSP.dko.json` adds the static analysis of a module's DSP code to `report`: SHARC counters are also added per loop.
//
// ADDR is a number (decimal or 0x hex), or `cpu:number` (`sharc:0xb80105`). A map is a `.addrmap.json` (docs/ADDRMAP.md); `--image X.syx` adds
// the ship map `X.addrmap.json` beside the image. `--loads` is a load table JSON; `--snapshot` a JSON object of runtime symbol to hex bytes.
import { execFileSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { existsSync, readFileSync, writeFileSync } from 'node:fs';
import path from 'node:path';
import { DkProf } from '../../packages/dk-session/src/dkprof.ts';

function readJson(file) {
  try {
    return JSON.parse(readFileSync(file, 'utf8'));
  } catch (e) {
    console.error(`wb: ${file}: ${e.message}`);
    process.exit(1);
  }
}

export async function loadProf(root, fail) {
  const wasm = path.join(root, 'apps/web/public/dkprof.wasm');
  if (!existsSync(wasm)) fail('apps/web/public/dkprof.wasm is missing (run: mise run wasm:dkprof)', 1);
  return DkProf.create(Uint8Array.from(readFileSync(wasm)));
}

function mapsOf(opts) {
  const files = [...(opts.map ?? [])];
  if (opts.image) files.push(opts.image.replace(/\.syx$/, '') + '.addrmap.json');
  return files.map((f) => readJson(f));
}

function parseAddr(text, defaultCpu, fail) {
  const [cpu, num] = text.includes(':') ? text.split(':') : [defaultCpu, text];
  if (cpu !== 'cf' && cpu !== 'sharc') fail(`bad CPU in ${text} (cf or sharc)`);
  const addr = Number(num);
  if (!Number.isSafeInteger(addr) || addr < 0) fail(`bad address ${text}`);
  return { cpu, addr };
}

function hexPair(spec, what, fail) {
  const at = spec.indexOf('=');
  if (at < 0) fail(`${what} ID=HEX`);
  return [spec.slice(0, at), Number(`0x${spec.slice(at + 1).replace(/^0x/, '')}`)];
}

/** Print an answer: the whole JSON, or the `text` view the wasm made. */
function show(answer, format) {
  if (format === 'json') console.log(JSON.stringify(answer));
  else process.stdout.write(answer.text);
}

export async function profCommand({ cmd, sub, rest, opts, root, fail }) {
  const format = opts.format ?? 'text';
  if (format !== 'json' && format !== 'text') fail('--format is json or text');
  const prof = cmd === 'profile' && sub === 'record' ? null : await loadProf(root, fail);
  if (cmd === 'addrmap' && sub === 'lookup') {
    if (rest.length === 0) fail('addrmap lookup ADDR... [--map F]...');
    const maps = mapsOf(opts);
    if (maps.length === 0) fail('addrmap lookup needs a map: --map F or --image X.syx');
    const req = { maps, addrs: rest.map((a) => parseAddr(a, opts.cpu ?? 'cf', fail)), ret: Boolean(opts.ret), format };
    if (opts.loads) req.loads = readJson(opts.loads);
    if (opts.snapshot) req.snapshot = readJson(opts.snapshot);
    for (const l of opts.load ?? []) {
      const [id, base] = hexPair(l, '--load', fail);
      (req.loads ??= { modules: [] }).modules.push({ id, base });
    }
    for (const d of opts['dsp-base'] ?? []) {
      const [id, pc] = hexPair(d, '--dsp-base', fail);
      (req.dsp_base ??= {})[id] = pc;
    }
    show(prof.addrLookup(req), format);
    return 0;
  }
  if (cmd === 'module' && sub === 'analyse') {
    if (rest.length !== 2) fail('module analyse MODULE.dkm DSP.dko.json');
    const [dkm, dko] = rest.map((f) => Uint8Array.from(readFileSync(f)));
    show(prof.moduleAnalyse(dkm, dko, format === 'text'), format);
    return 0;
  }
  if (cmd === 'profile' && sub === 'report') {
    if (rest.length !== 1) fail('profile report FILE [--top N]');
    const req = { profile: readJson(rest[0]), maps: mapsOf(opts), format };
    if (opts.top !== undefined) req.top = Number(opts.top);
    if (opts.loads) req.loads = readJson(opts.loads);
    for (const spec of opts.analyse ?? []) {
      const at = spec.indexOf('=');
      const [dkm, dko] = (at < 0 ? '' : spec.slice(at + 1)).split(':');
      if (at < 0 || !dkm || !dko) fail('--analyse ID=MODULE.dkm:DSP.dko.json');
      (req.analyses ??= {})[spec.slice(0, at)] = prof.moduleAnalyse(
        Uint8Array.from(readFileSync(dkm)),
        Uint8Array.from(readFileSync(dko)),
      );
    }
    show(prof.profileReport(req), format);
    return 0;
  }
  if (cmd === 'profile' && sub === 'record') return record({ opts, root, fail });
  fail(
    'usage: wb addrmap lookup | module analyse | profile report | profile record | profile spans (see tools/devtools/wb_prof.mjs)',
  );
}

const PHASES = ['early', 'late', 'last'];

/** `profile spans`: the runtime span counters of a device (dk-session `profileAll`) as a dk.profile/1 with `spans`, and a table. */
export async function profSpans({ opts, session, fail }) {
  if (opts.op) await session.profileState(opts.op);
  const all = await session.profileAll();
  const modules = await session.modules();
  const hz = all.hz;
  const nameOf = (s) => {
    if (s.kind === 'module') return modules.find((m) => m.index === s.index)?.name ?? `module ${s.index}`;
    if (s.kind === 'phase') return PHASES[s.index] ?? `phase ${s.index}`;
    return `poll ${s.index}`;
  };
  const ownerOf = (s) => (s.kind === 'module' ? s.index : s.kind === 'phase' ? 0x100 | s.index : 0x200 | s.index);
  const spans = all.spans.map((s) => ({
    owner: ownerOf(s),
    kind: s.kind,
    name: nameOf(s),
    calls: Number(s.calls),
    sum_ticks: Number(s.sumTicks),
    max_ticks: Number(s.maxTicks),
  }));
  if (spans.length > 0 && !(hz > 0)) fail('the device reported spans without a clock rate', 1);
  const profile = {
    schema: 'dk.profile/1',
    run: { id: `dev-${new Date().toISOString().replace(/[-:]/g, '').slice(0, 15)}`, target: 'unknown' },
    clock: { source: 'runtime.span', unit: 'tick', sampling: 'runtime span counters (calls, sum and max of ticks)' },
    raw: [],
    metrics: [],
    markers: [],
    loss: [],
    overhead: [],
    ...(spans.length > 0 ? { spans, span_clock_hz: hz } : {}),
  };
  if (opts.out) writeFileSync(opts.out, `${JSON.stringify(profile)}\n`);
  console.log(`${all.running ? 'running' : 'stopped'}, ${hz} Hz`);
  console.log('name\tcalls\ttotal ms\tmean us\tmax us');
  for (const s of spans) {
    const us = (t) => (t * 1e6) / hz;
    const mean = s.calls > 0 ? us(s.sum_ticks) / s.calls : 0;
    console.log(
      `${s.name}\t${s.calls}\t${(us(s.sum_ticks) / 1000).toFixed(3)}\t${mean.toFixed(1)}\t${us(s.max_ticks).toFixed(1)}`,
    );
  }
  if (opts.out) console.log(`${opts.out}: ${spans.length} spans`);
  return 0;
}

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const PATHS = ['ram_clear', 'run_fast', 'step_once', 'idle_skip'];

/** Rows `[hex pc, weight]` of two reports as counters of the difference. Rows that fell out of the top-N of the later report are lost. */
function deltaRows(before, after, series) {
  const base = new Map((before ?? []).map(([pc, w]) => [pc, w]));
  return (after ?? [])
    .map(([pc, w]) => ({ cpu: 'cf', addr: Number(pc), count: w - (base.get(pc) ?? 0), series }))
    .filter((r) => r.count > 0);
}

/** The load table of the running image: the symbols the ship map names, read from guest memory and decoded by dkprof. */
async function readLoads(prof, client, ship, plan) {
  const reads = prof.loadTable({ maps: [ship] }).reads;
  const snapshot = {};
  for (const { symbol, len } of reads) {
    const found = Object.entries(plan.symbols).filter(([k]) => k.endsWith(`:${symbol}`));
    if (found.length !== 1) throw new Error(`symbol ${symbol}: ${found.length} addresses in the plan`);
    const base = found[0][1];
    let hex = '';
    for (let i = 0; i < len; i += 4) {
      const w = (await client.memPeek(base + i)) ?? 0;
      hex += (w >>> 0).toString(16).padStart(8, '0');
    }
    snapshot[symbol] = hex.slice(0, len * 2);
  }
  return prof.loadTable({ maps: [ship], snapshot });
}

/** The identity of the run: id, target and, with `--image`, the hashes and the load table read from the running emulator. */
async function runInfo({ opts, root, fail }, client) {
  const run = { id: `emu-${new Date().toISOString().replace(/[-:]/g, '').slice(0, 15)}`, target: 'unknown' };
  try {
    run.source_revision = execFileSync('git', ['rev-parse', 'HEAD'], { cwd: root, encoding: 'utf8' }).trim();
  } catch {
    // not a git checkout: leave it out
  }
  if (opts.image) {
    const ship = readJson(opts.image.replace(/\.syx$/, '') + '.addrmap.json');
    const plan = readJson(opts.image.replace(/\.syx$/, '') + '.plan.json');
    run.target = ship.key.device ?? run.target;
    run.firmware_sha256 = ship.key.image_sha256;
    run.runtime_sha256 = ship.key.output_sha256 ?? createHash('sha256').update(readFileSync(opts.image)).digest('hex');
    run.module_sha256 = ship.key.modules ?? [];
    if (ship.key.loader) run.loads = await readLoads(await loadProf(root, fail), client, ship, plan);
  }
  return run;
}

async function connectDaemon({ opts, root, fail }) {
  const { connect, readSocket } = await import('../../packages/control-client/src/index.ts');
  const runDir = path.resolve(root, opts['run-dir'] ?? 'out/native/run');
  const sock = readSocket(runDir);
  if (!sock)
    fail(
      `no emulator socket in ${runDir} (start one: mise run emu:serve; the daemon needs the cf-profile or sharc-profile cargo feature)`,
      1,
    );
  return connect(sock);
}

/** `--cpu sharc`: the DSP counts of `sharc.profile` as a profile. */
async function recordSharc(ctx) {
  const { opts, fail } = ctx;
  const seconds = Number(opts.seconds ?? 0);
  if (!opts.out || (!opts['since-boot'] && !(seconds > 0)))
    fail('profile record --cpu sharc --seconds N --out FILE [--image X.syx] [--run-dir DIR] [--since-boot]');
  const client = await connectDaemon(ctx);
  try {
    if (!opts['since-boot']) {
      await client.call('sharc.profile', { enable: true });
      await sleep(seconds * 1000);
    }
    const a = await client.call('sharc.profile');
    const run = await runInfo(ctx, client);
    const profile = {
      schema: 'dk.profile/1',
      run,
      clock: a.clock,
      raw: a.raw,
      metrics: [
        { name: 'sharc.blocks', value: a.blocks, unit: 'blocks' },
        { name: 'sharc.insn.executed', value: a.icount, unit: 'insn' },
        { name: 'sharc.insn.idle_skipped', value: a.idle_skipped, unit: 'insn' },
        { name: 'sharc.insn.counted.fast', value: a.counted.fast, unit: 'insn' },
        { name: 'sharc.insn.counted.block', value: a.counted.block, unit: 'insn' },
        { name: 'sharc.insn.counted.interpreted', value: a.counted.interpreted, unit: 'insn' },
        {
          name: 'sharc.coverage',
          unit: 'ratio',
          na_reason:
            'sharc.insn.executed and the per-PC counts do not reconcile in the DN2 proof run (open, profiling plan P4)',
        },
      ],
      markers: [{ name: 'record.start', at: 0 }],
      loss: [],
      overhead: [
        {
          name: 'sharc-profile.feature',
          unit: 'ratio',
          na_reason: 'not measured: needs a paired run without the feature',
        },
      ],
    };
    writeFileSync(opts.out, `${JSON.stringify(profile)}\n`);
    console.log(`${opts.out}: ${a.raw.length} counters over ${a.blocks} DSP blocks`);
  } finally {
    client.close();
  }
  return 0;
}

async function record(ctx) {
  const { opts, fail } = ctx;
  const cpu = opts.cpu ?? 'cf';
  if (cpu === 'sharc') return recordSharc(ctx);
  if (cpu !== 'cf') fail('--cpu is cf or sharc');
  const seconds = Number(opts.seconds);
  if (!(seconds > 0) || !opts.out)
    fail('profile record --seconds N --out FILE [--image X.syx] [--run-dir DIR] [--since-boot] [--cpu cf|sharc]');
  const client = await connectDaemon(ctx);
  try {
    const before = opts['since-boot'] ? { paths: [] } : await client.call('cf.profile');
    await sleep(seconds * 1000);
    const after = await client.call('cf.profile');
    const raw = [
      ...deltaRows(before.pc_samples, after.pc_samples, 'pc'),
      ...deltaRows(before.idle_pc, after.idle_pc, 'idle_pc'),
      ...deltaRows(before.exit_pcs, after.exit_pcs, 'exit_pc'),
    ];
    const run = await runInfo(ctx, client);
    const metrics = [];
    for (const p of PATHS) {
      const a = after.paths.find((x) => x.path === p) ?? {};
      const b = before.paths.find((x) => x.path === p) ?? {};
      metrics.push(
        { name: `path.${p}.instructions`, value: (a.instructions ?? 0) - (b.instructions ?? 0), unit: 'insn' },
        { name: `path.${p}.calls`, value: (a.calls ?? 0) - (b.calls ?? 0), unit: 'calls' },
        { name: `path.${p}.host_ns`, value: (a.host_ns ?? 0) - (b.host_ns ?? 0), unit: 'ns' },
      );
    }
    metrics.push({
      name: 'sharc.pc',
      unit: 'insn',
      na_reason: 'not recorded here: wb profile record --cpu sharc records the DSP counts (sharc.profile)',
    });
    const profile = {
      schema: 'dk.profile/1',
      run,
      clock: {
        source: 'emu.cf.insn',
        unit: 'insn',
        sampling:
          'pc: every 64 insns in run_fast, every insn in step_once; idle_pc: weighted by insns skipped; exit_pc: run_fast exits (a count of exits, not insns)',
      },
      raw,
      metrics,
      markers: [{ name: 'record.start', at: 0 }],
      loss: [
        {
          series: 'all',
          reason:
            'cf.profile keeps the 400 heaviest rows of its cumulative tables; rows that fell out of the later report are lost from the difference',
        },
      ],
      overhead: [
        {
          name: 'cf-profile.feature',
          unit: 'ratio',
          na_reason: 'not measured: needs a paired run without the feature',
        },
      ],
    };
    writeFileSync(opts.out, `${JSON.stringify(profile)}\n`);
    console.log(`${opts.out}: ${raw.length} counters over ${seconds} s`);
  } finally {
    client.close();
  }
  return 0;
}
