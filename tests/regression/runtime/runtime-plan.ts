import fs from 'node:fs';
import path from 'node:path';
import {
  validatePreparedRuntimeImage,
  resolveInputs,
  type Session,
  type SessionOptions,
  type PreparedRuntimeImage,
} from '@digi/test-support';
import { patchkit } from './patchkit.ts';

export interface RuntimePlan {
  base: { device: string; version: string; syx_sha256: string };
  modules: string[];
  symbols: Record<string, number>;
  output: { syx_sha256: string } | null;
}

/** Same exact-one-address semantics as pk_harness.plan_symbol, including Rust statics. */
export function planSymbol(plan: RuntimePlan, name: string): number {
  const addresses = new Set(
    Object.entries(plan.symbols)
      .filter(([key]) => key.endsWith(`:${name}`) || new RegExp(`\\d${name}$`).test(key))
      .map(([, value]) => value),
  );
  if (addresses.size !== 1) throw new Error(`symbol ${name}: ${addresses.size} addresses in the plan`);
  return [...addresses][0]!;
}

export function validateRuntimePlan(
  plan: RuntimePlan,
  session: Pick<Session, 'options' | 'loadResult'> & Partial<Pick<Session, 'preparedRuntimeImage'>>,
): void {
  const identity = session.loadResult.runtime;
  // Cache hits report plain IDs in root order; misses report tags in dependency order.
  const moduleIds = (modules: string[]) => modules.map((module) => module.split('@', 1)[0]).sort();
  const prepared = session.preparedRuntimeImage;
  if (prepared) {
    if (
      plan.base.device !== session.options.device ||
      plan.base.version !== session.options.firmware ||
      plan.base.syx_sha256 !== prepared.baseSha256 ||
      JSON.stringify(moduleIds(plan.modules)) !== JSON.stringify(moduleIds(prepared.modules))
    )
      throw new Error('runtime patch plan does not match the prepared image identity and module membership');
  } else if (
    !identity?.patched ||
    identity.set !== (session.options.runtime?.patchSet ?? 'emulator') ||
    plan.base.device !== session.options.device ||
    plan.base.version !== session.options.firmware ||
    plan.base.syx_sha256 !== identity.base_sha256 ||
    JSON.stringify(moduleIds(plan.modules)) !== JSON.stringify(moduleIds(identity.modules))
  ) {
    throw new Error('runtime patch plan does not match the loaded runtime identity and module membership');
  }
  // Exact loaded-image identity proves version, ordering and symbol layout despite report differences.
  const loadedSha256 = session.loadResult.syx_sha256;
  if (
    typeof loadedSha256 !== 'string' ||
    !/^[a-f0-9]{64}$/.test(loadedSha256) ||
    plan.output?.syx_sha256 !== loadedSha256 ||
    (prepared !== undefined && prepared.syxSha256 !== loadedSha256)
  ) {
    throw new Error('runtime patch plan output hash does not match the loaded runtime image');
  }
}

export interface PreparedRuntimePlan {
  plan: RuntimePlan;
  planFile: string;
  syx: string;
  preparedImage?: PreparedRuntimeImage;
}

/** Offline plan preparation also supplies startup instrumentation before a daemon exists. */
export async function prepareRuntimePlanOffline(
  options: Pick<SessionOptions, 'device' | 'firmware' | 'runtime'>,
  directory: string,
  artifacts: string,
  symbols: string[],
  selection?: { modules: string[] },
): Promise<PreparedRuntimePlan> {
  const { device, firmware } = options;
  if (selection && (selection.modules.length === 0 || selection.modules.some((id) => !id || id.includes(',')))) {
    throw new Error('explicit runtime module roots must be a nonempty list of module IDs');
  }
  fs.mkdirSync(directory, { recursive: true });
  const syx = path.join(directory, `runtime-${device}.syx`);
  const planFile = syx.replace(/\.syx$/, '.plan.json');
  const patch = await patchkit(
    [
      'apply',
      '--firmware',
      resolveInputs(device, firmware).syx,
      ...(selection ? ['--modules', selection.modules.join(',')] : ['--set', options.runtime?.patchSet ?? 'emulator']),
      '-o',
      syx,
      '--plan',
      planFile,
    ],
    { timeout: 30_000 },
  );
  fs.writeFileSync(path.join(artifacts, 'patch.log'), `${patch.stdout}\n${patch.stderr}`);
  const plan: RuntimePlan = JSON.parse(fs.readFileSync(planFile, 'utf8'));
  for (const symbol of symbols) planSymbol(plan, symbol);
  if (selection) {
    const moduleIds = plan.modules.map((module) => module.split('@', 1)[0]!);
    if (
      plan.base.device !== device ||
      plan.base.version !== firmware ||
      new Set(moduleIds).size !== moduleIds.length ||
      selection.modules.some((id) => !moduleIds.includes(id)) ||
      !plan.output?.syx_sha256
    )
      throw new Error('explicit runtime plan does not match the requested device, firmware and module roots');
    const preparedImage: PreparedRuntimeImage = {
      syx,
      baseSha256: plan.base.syx_sha256,
      syxSha256: plan.output.syx_sha256,
      modules: moduleIds,
    };
    validatePreparedRuntimeImage(preparedImage, resolveInputs(device, firmware).syx);
    return { plan, planFile, syx, preparedImage };
  }
  return { plan, planFile, syx };
}

/** Loaded-image identity is checked separately from offline preparation. */
export async function prepareRuntimePlan(session: Session, artifacts: string, symbols: string[]): Promise<RuntimePlan> {
  const { plan } = await prepareRuntimePlanOffline(session.options, session.runDir, artifacts, symbols);
  validateRuntimePlan(plan, session);
  return plan;
}
