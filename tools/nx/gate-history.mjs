// Reads Nx's own task history (task_history joined with task_details in the workspace database of this
// worktree). Nx records a run when the command ends, so a gate that was killed leaves no record.
// Cache replays are recorded too; only runs with status success or failure count here.
//   node tools/nx/gate-history.mjs [--days N] [--top N] [--since EPOCH_MS]
import { readdirSync, realpathSync } from 'node:fs';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { DatabaseSync } from 'node:sqlite';

const dataDir = () => process.env.NX_WORKSPACE_DATA_DIRECTORY || join(process.cwd(), '.nx/workspace-data');

export function readRuns(sinceMs = 0) {
  const file = readdirSync(dataDir()).find((name) => name.endsWith('.db'));
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

if (process.argv[1] && realpathSync(process.argv[1]) === fileURLToPath(import.meta.url)) {
  const args = process.argv.slice(2);
  const flag = (name, fallback) => Number(args[args.indexOf(name) + 1] || fallback);
  const since = args.includes('--since') ? flag('--since') : Date.now() - flag('--days', 14) * 86400000;
  console.log(report(readRuns(since), flag('--top', 10)));
}
