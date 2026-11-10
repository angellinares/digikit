// Reads Nx's own task history (task_history joined with task_details in the workspace database of this
// worktree). Nx records a run when the command ends, so a gate that was killed leaves no record.
// Cache replays are recorded too; only runs with status success or failure count here.
//   node tools/nx/gate-history.mjs [--days N] [--top N] [--since EPOCH_MS]
//   node tools/nx/gate-history.mjs --trace <gate run dir>   (summary and trace.json of one gate run)
import { existsSync, readFileSync, readdirSync, realpathSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { DatabaseSync } from 'node:sqlite';

const dataDir = () => process.env.NX_WORKSPACE_DATA_DIRECTORY || join(process.cwd(), '.nx/workspace-data');

export function readRuns(sinceMs = 0) {
  // The newest schema database: `<id>-v<N>.db` with the highest N (older schemas stay beside it).
  const version = (name) => Number(/-v(\d+)\.db$/.exec(name)?.[1] ?? -1);
  const file = readdirSync(dataDir())
    .filter((name) => version(name) >= 0)
    .sort((a, b) => version(b) - version(a))[0];
  if (!file) return [];
  const db = new DatabaseSync(join(dataDir(), file), { readOnly: true });
  try {
    return db
      .prepare(
        `SELECT d.project, d.target, d.configuration, h.hash, h.status, h.code, h.start, h."end" - h.start AS ms
         FROM task_history h JOIN task_details d ON d.hash = h.hash
         WHERE h.status IN ('success', 'failure') AND h.start >= ? ORDER BY h.start`,
      )
      .all(sinceMs)
      .map((row) => ({
        ...row,
        id: `${row.project}:${row.target}${row.configuration ? `:${row.configuration}` : ''}`,
      }));
  } finally {
    db.close();
  }
}

const seconds = (ms) => `${(ms / 1000).toFixed(1)} s`;
const median = (values) => [...values].sort((a, b) => a - b)[Math.floor(values.length / 2)];

export const slowest = (runs, top) => {
  const best = new Map();
  for (const run of runs) if (!best.has(run.id) || run.ms > best.get(run.id).ms) best.set(run.id, run);
  return [...best.values()].sort((a, b) => b.ms - a.ms).slice(0, top);
};

export const failed = (runs) => {
  const last = new Map();
  for (const run of runs) if (run.code !== 0) last.set(run.id, run);
  return [...last.values()].sort((a, b) => b.start - a.start);
};

// The latest run against the median of up to ten earlier runs of the same task.
export const slower = (runs, ratio = 1.5, minMs = 5000) => {
  const byId = new Map();
  for (const run of runs) byId.set(run.id, [...(byId.get(run.id) ?? []), run]);
  const out = [];
  for (const [id, list] of byId) {
    if (list.length < 4) continue;
    const latest = list.at(-1);
    const before = median(list.slice(-11, -1).map((run) => run.ms));
    if (latest.ms > before * ratio && latest.ms - before > minMs) out.push({ id, before, ms: latest.ms });
  }
  return out.sort((a, b) => b.ms - b.before - (a.ms - a.before));
};

export function report(runs, top = 10) {
  const lines = [`slowest tasks (${runs.length} runs)`];
  for (const run of slowest(runs, top)) lines.push(`  ${seconds(run.ms).padStart(9)}  ${run.id}`);
  const bad = failed(runs);
  lines.push(`failed (${bad.length})`);
  for (const run of bad.slice(0, top)) lines.push(`  ${new Date(run.start).toISOString()}  ${run.id}`);
  const slow = slower(runs);
  lines.push(`slower than the median of earlier runs (${slow.length})`);
  for (const run of slow.slice(0, top)) lines.push(`  ${seconds(run.before)} -> ${seconds(run.ms)}  ${run.id}`);
  return lines.join('\n');
}

const readJsonLines = (file) =>
  existsSync(file)
    ? readFileSync(file, 'utf8')
        .split('\n')
        .filter(Boolean)
        .flatMap((line) => {
          try {
            return [JSON.parse(line)];
          } catch {
            return []; // a torn line from a killed task
          }
        })
    : [];

// Summary of one gate run from its trace dir (nx-profile.json from Nx, slots-trace.json from emu-slot.mjs).
// Writes trace.json (Chrome trace format, open in ui.perfetto.dev): Nx task spans plus the slot wait and run spans.
export function traceSummary(dir, top = 10) {
  const profileFile = join(dir, 'nx-profile.json');
  const profile = existsSync(profileFile) ? JSON.parse(readFileSync(profileFile, 'utf8')) : [];
  const slots = readJsonLines(join(dir, 'slots-trace.json'));
  const tasks = profile.filter((event) => event.ph === 'X');
  const waitBy = new Map();
  for (const event of slots)
    if (event.name === 'wait') waitBy.set(event.tid, (waitBy.get(event.tid) ?? 0) + event.dur / 1e6);
  const sum = (name) =>
    slots.filter((event) => event.name === name).reduce((total, event) => total + event.dur / 1e6, 0);
  const rows = tasks.map((event) => {
    const total = event.dur / 1e6;
    const wait = Math.min(total, waitBy.get(`${event.args?.target?.project}:${event.args?.target?.target}`) ?? 0);
    return { id: event.name, total, wait, run: total - wait };
  });
  const starts = tasks.map((event) => event.ts);
  const ends = tasks.map((event) => event.ts + event.dur);
  const wall = starts.length ? (Math.max(...ends) - Math.min(...starts)) / 1e6 : 0;
  const minutes = (sec) => (sec / 60).toFixed(1);
  const lines = [
    `gate trace: wall ${minutes(wall)} min, ${rows.length} tasks, ${minutes(rows.reduce((t, r) => t + r.total, 0))} task-minutes`,
    `  emulator slots: wait ${minutes(sum('wait'))} min, run ${minutes(sum('run'))} min`,
    `  top ${top} tasks by run time (task time minus slot wait)`,
  ];
  for (const row of rows.sort((a, b) => b.run - a.run).slice(0, top))
    lines.push(
      `  ${seconds(row.run * 1000).padStart(9)}  ${row.id}${row.wait >= 1 ? ` (waited ${seconds(row.wait * 1000)})` : ''}`,
    );
  // Slot events use string pid/tid; trace viewers want numbers, so map them and name the threads.
  const slotPid = 1_000_000;
  const tids = new Map();
  const merged = [
    ...profile,
    { name: 'process_name', ph: 'M', pid: slotPid, tid: 0, ts: 0, args: { name: 'emulator slots' } },
  ];
  for (const event of slots) {
    if (!tids.has(event.tid)) {
      tids.set(event.tid, tids.size + 1);
      merged.push({
        name: 'thread_name',
        ph: 'M',
        pid: slotPid,
        tid: tids.get(event.tid),
        ts: 0,
        args: { name: event.tid },
      });
    }
    merged.push({ ...event, pid: slotPid, tid: tids.get(event.tid) });
  }
  writeFileSync(join(dir, 'trace.json'), JSON.stringify(merged));
  lines.push(`  trace: ${join(dir, 'trace.json')} (ui.perfetto.dev)`);
  return lines.join('\n');
}

if (process.argv[1] && realpathSync(process.argv[1]) === fileURLToPath(import.meta.url)) {
  const args = process.argv.slice(2);
  if (args[0] === '--trace') {
    console.log(traceSummary(args[1]));
    process.exit(0);
  }
  const flag = (name, fallback) => Number(args[args.indexOf(name) + 1] || fallback);
  const since = args.includes('--since') ? flag('--since') : Date.now() - flag('--days', 14) * 86400000;
  console.log(report(readRuns(since), flag('--top', 10)));
}
