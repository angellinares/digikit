import fs from 'node:fs';
import path from 'node:path';
import {
  validatePreparedRuntimeImage,
  repoRoot,
  resolveInputs,
  type Session,
  type SessionOptions,
  type PreparedRuntimeImage,
} from '@digi/test-support';
import { rootSets } from './image-roots.ts';
import { patchkitCovered } from './patchkit-exec.ts';

export interface RuntimePlan {
  base: { device: string; version: string; syx_sha256: string };
  modules: string[];
  symbols: Record<string, number>;
  /** What the patchkit wrote into the image: patches and placed objects (`kind: 'place'`, `what` 'object in <region>' or 'hook tail'). */
  writes?: {
    cpu: string;
    addr: number;
    len: number;
    kind: string;
    owner: string;
    what: string;
    code?: [number, number][];
  }[];
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

const shipMaps = new WeakMap<RuntimePlan, string>();
/** The address map of the image of `plan` (`<plan>.addrmap.json`, written beside the plan by `patchkit apply --addrmap`), remembered for every plan prepared here. */
export const shipMapOf = (plan: RuntimePlan): string | undefined => shipMaps.get(plan);

export interface PreparedRuntimePlan {
  plan: RuntimePlan;
  planFile: string;
  syx: string;
  preparedImage?: PreparedRuntimeImage;
}

/**
 * Offline plan preparation of the patch set (`apply --set`): its result is the image, plan and address map hashed in
 * dist/bytes/images.sha256, so this file does not count as running patchkit live. An explicit module list goes through
 * runtime-plan-modules.ts. Also supplies startup instrumentation before a daemon exists.
 */
export function prepareRuntimePlanOffline(
  options: Pick<SessionOptions, 'device' | 'firmware' | 'runtime'>,
  directory: string,
  artifacts: string,
  symbols: string[],
): Promise<PreparedRuntimePlan> {
  assertCovered(options);
  return prepareOfflineWith(patchkitCovered, options, directory, artifacts, symbols);
}

/** `apply --set` is covered by dist/bytes/images.sha256 only for a firmware and patch set that file has a line for (the acceptance cells). */
function assertCovered({ device, firmware, runtime }: Pick<SessionOptions, 'device' | 'firmware' | 'runtime'>): void {
  const set = runtime?.patchSet ?? 'emulator';
  const file = path.join(repoRoot(), 'dist/bytes/images.sha256');
  if (!fs.existsSync(file)) throw new Error(`missing ${file}: run nx run patchkit:device-images`);
  if (
    !fs
      .readFileSync(file, 'utf8')
      .split('\n')
      .some((line) => line.startsWith(`${device}-${firmware} ${set} image `))
  )
    throw new Error(
      `${device} ${firmware} with patch set ${set} is not in dist/bytes/images.sha256: add the firmware to CELLS in tools/nx/device-inputs.mjs, or run patchkit through runtime/patchkit.ts (a live test)`,
    );
}

/** What the offline preparation needs of the patch step: `patchkit apply ...`, or the copy of an image made ahead. */
export type PatchExec = (args: string[], opts: { timeout?: number }) => Promise<{ stdout: string; stderr: string }>;

/** The static `--modules` lists (image-roots.json) that patchkit:device-images patched ahead into dist/bytes/images/<device>/<id>.*. */
function listedRoots(device: string, modules: string[]): string {
  const found = rootSets().find(
    (set) => set.roots.join(',') === modules.join(',') && (!set.devices || set.devices.includes(device)),
  );
  if (!found)
    throw new Error(
      `module roots ${modules.join(',')} on ${device} are not in tests/regression/runtime/image-roots.json: add the list, or use runtime-plan-modules.ts for a list built at test time`,
    );
  return found.id;
}

/** The copy of the image, plan and address map made ahead for the listed roots: no patchkit runs. A missing file fails. */
const copyListed =
  (device: string): PatchExec =>
  async (args) => {
    const at = (flag: string) => args[args.indexOf(flag) + 1]!;
    const id = listedRoots(device, at('--modules').split(','));
    const from = path.join(repoRoot(), 'dist/bytes/images', device, id);
    const planFile = at('--plan');
    const copies: [string, string][] = [
      [`${from}.syx`, at('-o')],
      [`${from}.plan.json`, planFile],
      [`${from}.addrmap.json`, at('--addrmap')],
    ];
    for (const [source, target] of copies) {
      if (!fs.existsSync(source)) throw new Error(`missing ${source}: run nx run patchkit:device-images`);
      fs.copyFileSync(source, target);
    }
    return { stdout: `listed roots ${id}: copied from ${from}.*`, stderr: '' };
  };

/**
 * Offline plan preparation for a static list of module roots from image-roots.json: the image, plan and address map were made ahead
 * (hashed in dist/bytes/images.sha256), so this file does not count as running patchkit live. A list that is not in the file fails.
 */
export function prepareRuntimePlanListedOffline(
  options: Pick<SessionOptions, 'device' | 'firmware' | 'runtime'>,
  directory: string,
  artifacts: string,
  symbols: string[],
  selection: { modules: string[] },
): Promise<PreparedRuntimePlan> {
  return prepareOfflineWith(copyListed(options.device), options, directory, artifacts, symbols, selection);
}

/** The shared body: `exec` is the patchkit helper of the caller (runtime-plan-modules.ts passes the one that counts as live). */
export async function prepareOfflineWith(
  exec: PatchExec,
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
  const patch = await exec(
    [
      'apply',
      '--firmware',
      resolveInputs(device, firmware).syx,
      ...(selection ? ['--modules', selection.modules.join(',')] : ['--set', options.runtime?.patchSet ?? 'emulator']),
      '-o',
      syx,
      '--plan',
      planFile,
      '--addrmap',
      planFile.replace(/\.plan\.json$/, '.addrmap.json'),
    ],
    { timeout: 30_000 },
  );
  fs.writeFileSync(path.join(artifacts, 'patch.log'), `${patch.stdout}\n${patch.stderr}`);
  const plan: RuntimePlan = JSON.parse(fs.readFileSync(planFile, 'utf8'));
  for (const symbol of symbols) planSymbol(plan, symbol);
  shipMaps.set(plan, planFile.replace(/\.plan\.json$/, '.addrmap.json'));
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
