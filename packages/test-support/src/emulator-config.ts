/** Resolver for the emulator tier settings in config/emulator.json; the same rules as tools/emulator/emulator_config.py.
 * Precedence, lowest to highest: built-in defaults, the file, then environment variables. An explicit variable wins,
 * including an empty one. */

type Entry = { fast_regions?: string; blocktrans?: string; fallback_regions?: string };
export type EmulatorConfig = Record<string, unknown>;

export interface TierResolution {
  /** Env additions (names carry the slot suffix; nothing the environment already sets). */
  add: Record<string, string>;
  /** Region lists the environment sets itself (slot-suffixed names, empty values kept). */
  explicit: Record<string, string>;
  /** DIGI_EMU_FAST=0: no region list is added. */
  off: boolean;
  /** name -> where the value came from. */
  sources: Record<string, 'env' | 'file' | 'context'>;
  auto: string | undefined;
}

const LISTS = [
  ['SHARC_FAST_REGIONS', 'fast_regions'],
  ['SHARC_BLOCKTRANS', 'blocktrans'],
  ['SHARC_FAST_FALLBACK_REGIONS', 'fallback_regions'],
] as const;
const FALLBACK = 'SHARC_FAST_FALLBACK_REGIONS';

export interface TierOptions {
  /** Name in `_contexts` of the config file, or undefined for no context defaults. */
  context?: string;
  /** SHA-256 of the firmware, '' when unknown. */
  digest: string;
  /** '' for the first core, '_2' for the second. */
  slot?: string;
  environ?: NodeJS.ProcessEnv;
  config: EmulatorConfig;
  /** Never add the fallback list. */
  fallback?: boolean;
  /** An entry with `when: generic-core` applies only when set. */
  genericCore?: boolean;
}

export function resolveTiers(o: TierOptions): TierResolution {
  const environ = o.environ ?? process.env;
  const slot = o.slot ?? '';
  const entry = ((o.config[o.digest] as Entry | undefined) ?? {}) as Entry;
  const off = environ.DIGI_EMU_FAST === '0';
  const add: Record<string, string> = {};
  const explicit: Record<string, string> = {};
  const sources: TierResolution['sources'] = {};
  for (const [name, key] of LISTS) {
    const full = name + slot;
    if (name === FALLBACK && o.fallback === false) continue;
    if (environ[full] !== undefined) {
      explicit[full] = environ[full] as string;
      sources[full] = 'env';
      continue;
    }
    if (off) continue;
    if (
      name === FALLBACK &&
      (environ['SHARC_FAST_REGIONS' + slot] !== undefined || environ.SHARC_FAST_AUTO !== undefined)
    )
      continue;
    const value = entry[key];
    if (value) {
      add[full] = value;
      sources[full] = 'file';
    }
  }
  const contexts = (o.config._contexts ?? {}) as Record<string, Record<string, string>>;
  const { when, ...defaults } = (o.context ? contexts[o.context] : undefined) ?? {};
  if (when === undefined || (when === 'generic-core' && o.genericCore)) {
    for (const [name, value] of Object.entries(defaults)) {
      if (environ[name] !== undefined) sources[name] = 'env';
      else {
        add[name] = value;
        sources[name] = 'context';
      }
    }
  }
  if (environ.SHARC_FAST_AUTO !== undefined) sources.SHARC_FAST_AUTO = 'env';
  return { add, explicit, off, sources, auto: environ.SHARC_FAST_AUTO ?? add.SHARC_FAST_AUTO };
}

const count = (value: string | undefined) => (value ?? '').split(',').filter((x) => x.trim()).length;

/** One line for a log: the context, SHARC_FAST_AUTO, list sizes and which values the environment set. */
export function tiersLine(context: string, env: Record<string, string>, res: TierResolution): string {
  const fromEnv = Object.entries(res.sources)
    .filter(([, source]) => source === 'env')
    .map(([name]) => name)
    .sort();
  return (
    `tiers: context=${context} SHARC_FAST_AUTO=${res.auto === undefined ? '-' : JSON.stringify(res.auto)} ` +
    `regions=${count(env.SHARC_FAST_REGIONS)} fallback=${count(env[FALLBACK])} blocktrans=${count(env.SHARC_BLOCKTRANS)} ` +
    `from-env=${fromEnv.join(',') || '-'}`
  );
}
