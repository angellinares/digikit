import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import type { Session } from '@digi/test-support';
import { expect, onTestFinished } from 'vitest';

export function record(dir: string, name: string, value: unknown): void {
  fs.writeFileSync(path.join(dir, `${name}.json`), JSON.stringify(value, null, 2));
}

const artifactDirectories = new WeakMap<Session, Map<string, string>>();

export function liveArtifacts(session: Session, workflow: string): string {
  let workflows = artifactDirectories.get(session);
  if (!workflows) {
    workflows = new Map();
    artifactDirectories.set(session, workflows);
  }
  const existing = workflows.get(workflow);
  if (existing) return existing;
  const root = path.join(os.tmpdir(), 'digi-regression', workflow, session.options.device);
  fs.mkdirSync(root, { recursive: true });
  const dir = fs.mkdtempSync(path.join(root, 'run-'));
  workflows.set(workflow, dir);
  console.info('runtime artifacts:', dir);
  try {
    // Removed when the requesting test passes; kept on failure, and when called outside a test.
    onTestFinished(({ task }) => {
      workflows.delete(workflow);
      if (task.result?.state === 'pass') fs.rmSync(dir, { recursive: true, force: true });
      else console.info('artifacts kept:', dir);
    });
  } catch {
    // Outside a test: keep the directory.
  }
  return dir;
}

export function liveCheck(name: string, ok: unknown, detail: unknown = ''): void {
  expect(Boolean(ok), `${name}: ${String(detail)}`).toBe(true);
}
