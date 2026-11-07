#!/usr/bin/env node
// Throughput of the digikit upload (BEGIN/DATA/END) and VERIFY against a running emulator, through its control socket, split into what the emulator costs
// (guest instructions, the real-time factor of the run) and what the protocol costs (round trips, polling). A dev tool: tools/devtools/upload_bench.py starts
// the daemon and runs this. By hand:
//
//     node tools/devtools/upload_bench.mjs SOCKET [--bytes N] [--chunk N] [--runs N] [--verify-piece N] [--depth N] [--path P] [--check-sizes N,N,...]
//
// One JSON line per run on stdout: wall seconds and MB/s of the upload and of VERIFY, guest instructions per file byte, the real-time factor.
// With `--check-sizes` instead one line per file size and no timing: the hash and 8-in-7 paths of the runtime against the host's file hash.
import { randomBytes } from 'node:crypto';
import { DkSession, fileHashHex } from '../../packages/dk-session/src/index.ts';
import { connect } from './emu_midi.mjs';

const [socketPath, ...flags] = process.argv.slice(2);
const flag = (name, dflt) => (flags.includes(name) ? flags[flags.indexOf(name) + 1] : dflt);
if (!socketPath) {
  console.error(
    'usage: node tools/devtools/upload_bench.mjs SOCKET [--bytes N] [--chunk N] [--runs N] [--verify-piece N] [--depth N] [--path P] [--check-sizes N,N,...]',
  );
  process.exit(2);
}
const bytes = Number(flag('--bytes', 8_000_000));
const chunk = Number(flag('--chunk', 32768));
const runs = Number(flag('--runs', 1));
const depth = Number(flag('--depth', 0)); // DATA requests in flight; 0: the transport's default
const piece = Number(flag('--verify-piece', 1 << 20));
const path = flag('--path', 'bench/file');

const sizes = flag('--check-sizes', null);
const emu = await connect(socketPath);
if (sizes !== null) {
  // correctness instead of speed: files of the given sizes (a comma list: the SHA-256 block edges 55, 56, 63, 64, 65, the 8-in-7 group, a chunk and one over)
  // are uploaded in the chunk size asked for, and VERIFY (the device's hash of what the card holds) is compared with the host's, whole and in ranges
  const session = new DkSession(emu.transport, { timeoutMs: 60000, chunk });
  let bad = 0;
  for (const n of sizes.split(',').map(Number)) {
    const data = Uint8Array.from(randomBytes(n));
    const want = fileHashHex(data);
    await session.put(`check/f${n}`, data);
    const whole = await session.verify(`check/f${n}`);
    let ranged = true;
    for (let off = 0; off < n; off += 4096) {
      const part = data.subarray(off, Math.min(n, off + 4096));
      if ((await session.verify(`check/f${n}`, off, part.length)) !== fileHashHex(part)) ranged = false;
    }
    const ok = whole === want && ranged;
    if (!ok) bad++;
    console.log(JSON.stringify({ size: n, ok, whole: whole === want, ranged }));
    await session.delete(`check/f${n}`);
  }
  emu.close();
  process.exit(bad ? 1 : 0);
}
const snap = async () => {
  const s = await emu.call('status');
  return {
    t: performance.now(),
    icount: s.cf.icount,
    idle: s.cf.idle_fast_forwarded_instructions,
    audio: s.audio?.source_audio_seconds ?? null,
    cpu: s.controller_cpu_ns / 1e9,
    dsp: s.audio?.dsp_tiers?.worker_cpu_s ?? null,
    stalls: s.heartbeat?.stall_count ?? null,
    underruns: s.heartbeat?.underrun_count ?? null,
  };
};
const rate = (a, b) => {
  const wall = (b.t - a.t) / 1000;
  const busy = b.icount - a.icount - (b.idle - a.idle);
  return {
    wall_s: +wall.toFixed(3),
    insn: b.icount - a.icount,
    busy_insn: busy,
    audio_s: b.audio === null ? null : +(b.audio - a.audio).toFixed(3),
    cf_cpu_s: +(b.cpu - a.cpu).toFixed(3),
    dsp_cpu_s: b.dsp === null ? null : +(b.dsp - a.dsp).toFixed(3),
    stall_count: b.stalls,
    underrun_count: b.underruns,
  };
};

// the transport of emu_midi.mjs, timed: where a request spends its wall time
const t = emu.transport;
const stat = { send_ms: 0, sends: 0, poll_ms: 0, polls: 0, empty: 0, sleep_ms: 0 };
const send = t.send.bind(t);
t.send = async (m) => {
  const t0 = performance.now();
  await send(m);
  stat.send_ms += performance.now() - t0;
  stat.sends++;
};
const recv = t.recv.bind(t);
t.recv = async (ms, sig) => {
  const t0 = performance.now();
  const r = await recv(ms, sig);
  stat.poll_ms += performance.now() - t0;
  stat.polls++;
  return r;
};

// the firmware's own load with nothing to do (instructions per emulated second that are not skipped idle passes): what the guest runs besides the
// work asked of it; `work_insn_per_byte` below is the instructions of a phase less this load over the phase's emulated time
const idle0 = await snap();
await new Promise((r) => setTimeout(r, 4000));
const idle1 = await snap();
const bg = (idle1.icount - idle0.icount - (idle1.idle - idle0.idle)) / (idle1.audio - idle0.audio);
const work = (r, bytes) => +((r.busy_insn - bg * r.audio_s) / bytes).toFixed(1);
const session = new DkSession(t, { timeoutMs: 60000, chunk, ...(depth > 0 ? { depth } : {}) });
const info = await session.info();
const data = Uint8Array.from(randomBytes(bytes));
for (let run = 0; run < runs; run++) {
  for (const k of Object.keys(stat)) stat[k] = 0;
  const a = await snap();
  let nChunks = 0;
  let putError = null;
  await session.put(path, data, () => nChunks++).catch((e) => (putError = String(e.message)));
  const b = await snap();
  const up = rate(a, b);
  const putStat = { ...stat };
  let ok = true;
  const c = await snap();
  try {
    for (let off = 0; off < data.length; off += piece) {
      const part = data.subarray(off, Math.min(data.length, off + piece));
      const got = await session.verify(path, off, part.length);
      if (got !== fileHashHex(part)) ok = false;
    }
  } catch {
    ok = false;
  }
  const d = await snap();
  const ver = rate(c, d);
  const mb = bytes / 1e6;
  console.log(
    JSON.stringify({
      run,
      bytes,
      chunk,
      chunks: nChunks,
      put_error: putError,
      free_before: info.free_bytes,
      bg_insn_per_audio_s: Math.round(bg),
      upload: {
        ...up,
        MBps: +(mb / up.wall_s).toFixed(3),
        insn_per_byte: +(up.insn / bytes).toFixed(1),
        work_insn_per_byte: work(up, bytes),
        busy_insn_per_byte: +(up.busy_insn / bytes).toFixed(1),
        rtf: up.audio_s === null ? null : +(up.audio_s / up.wall_s).toFixed(3),
      },
      transport: {
        ...Object.fromEntries(Object.entries(putStat).map(([k, v]) => [k, Math.round(v)])),
        ms_per_chunk: +((up.wall_s * 1000) / nChunks).toFixed(1),
      },
      verify: {
        ...ver,
        ok,
        MBps: +(mb / ver.wall_s).toFixed(3),
        insn_per_byte: +(ver.insn / bytes).toFixed(1),
        work_insn_per_byte: work(ver, bytes),
        busy_insn_per_byte: +(ver.busy_insn / bytes).toFixed(1),
        rtf: ver.audio_s === null ? null : +(ver.audio_s / ver.wall_s).toFixed(3),
      },
    }),
  );
  await session.delete(path).catch(() => {});
}
emu.close();
process.exit(0);
