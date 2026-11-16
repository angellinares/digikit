/**
 * The refusal fixtures of the loader and the boot plan (docs/plans/runtime-and-services.md section 8, item R11): for every load status of `[load.status]`
 * one proof that a module is refused with that code (must fail) and one that the same module loads without the fault (must pass), or the reason why a code
 * has no module-file fixture. `refusal-table.test.ts` fails when a code has no row, when a row names a test that is gone, and when an exempt code gives no reason.
 *
 * A proof is a device test (`*.device.test.ts`: a packed or patched `.dkm` through the real loader, asserting the module's state and code) or, for a code that no
 * file can cause, a host test of the layer that returns it. `kind` says which.
 */
export interface Proof {
  /** The file, relative to the repository root. */
  file: string;
  /** A substring of the title of the test in that file. */
  test: string;
}

export type Row =
  | { fail: Proof; pass: Proof }
  | {
      /** `reserved`: the code is never returned. `not-a-load`: a module file cannot cause it (a store, selection, card or files result). `host`: only a host test of the layer reaches it. `open`: no fixture yet. */
      kind: 'reserved' | 'not-a-load' | 'host' | 'open';
      reason: string;
      /** Host tests (`kind: 'host'`) or other tests that return the code. */
      covered?: Proof[];
    };

const REFUSAL = 'modules/core/runtime/tests/refusal/refusal.device.test.ts';
const CAPACITY = 'modules/core/runtime/tests/capacity/capacity.device.test.ts';
const COMMIT = 'modules/core/runtime/tests/capacity/commit.device.test.ts';
const NEEDS = 'modules/core/apps/digikit-menu/tests/needs.device.test.ts';
const SAFE = 'modules/core/runtime/tests/safe-mode.device.test.ts';
const COLLECT = 'modules/core/runtime/src/rs/src/collect/tests.rs';
const CAP_RS = 'modules/core/runtime/sdk/src/capacity/tests.rs';
const STORE_RS = 'modules/core/runtime/sdk/src/store/tests.rs';
const CLEANUP = 'modules/core/runtime/src/rs/src/cleanup_tests.rs';
const CONTRACT = 'modules/core/runtime/src/rs/src/contract_tests.rs';
const LFO_DEVICE = 'modules/core/runtime/tests/lfo/lfo.device.test.ts';
const SAMPLES_RS = 'modules/core/runtime/sdk/src/sample_tracks/tests.rs';

const PROVIDERS = 'modules/core/runtime/tests/services/providers.device.test.ts';
const prov = (test: string): Proof => ({ file: PROVIDERS, test });
const mine = (test: string): Proof => ({ file: REFUSAL, test });
const loads = mine('the good module loads and is ACTIVE');

/** The row of each `[load.status]` name. */
export const ROWS: Record<string, Row> = {
  E_NOT_FOUND: { fail: mine('a selected file the card does not have is E_NOT_FOUND'), pass: loads },
  E_BAD_STORE: { kind: 'not-a-load', reason: 'the store header is checked before any module is selected', covered: [{ file: STORE_RS, test: 'E_BAD_STORE' }] },
  E_READ: { kind: 'open', reason: 'no boot rig makes the firmware block read fail while a module loads', covered: [{ file: STORE_RS, test: 'E_READ' }] },
  E_SHA: { fail: mine('a changed sha256 is E_SHA'), pass: loads },
  E_FORMAT: { fail: mine('a wrong magic is E_FORMAT'), pass: loads },
  E_RUNTIME: { kind: 'reserved', reason: 'never returned: E_CONTRACT says a module needs another runtime' },
  E_IMPORT: { fail: { file: NEEDS, test: 'usb-fast without core/usb-bulk' }, pass: { file: 'modules/tools/usb-fast/tests/usb-fast.device.test.ts', test: 'the speed switches' } },
  E_NOMEM: { fail: { file: CAPACITY, test: 'arena: filled to the last byte' }, pass: { file: CAPACITY, test: 'arena: filled to the last byte' } },
  E_INIT: { fail: mine('an init that returns non-zero is E_INIT'), pass: loads },
  E_LOADED: { fail: { file: CAPACITY, test: 'files: a file of the staging buffer size' }, pass: { file: CAPACITY, test: 'files: a file of the staging buffer size' } },
  E_BUSY: { kind: 'not-a-load', reason: 'a task is still inside the module: only a teardown returns it, never the boot', covered: [{ file: CLEANUP, test: 'E_BUSY' }] },
  E_TOO_BIG: { fail: { file: CAPACITY, test: 'files: a file of the staging buffer size' }, pass: { file: CAPACITY, test: 'files: a file of the staging buffer size' } },
  E_TABLE_FULL: { kind: 'host', reason: 'the selection holds as many paths as there are module records, so the selection is refused first', covered: [{ file: CAP_RS, test: 'E_TABLE_FULL' }] },
  E_LOST: { kind: 'not-a-load', reason: 'a sample file result', covered: [{ file: SAMPLES_RS, test: 'E_LOST' }] },
  E_POOL: { kind: 'not-a-load', reason: 'a sample pool result', covered: [{ file: SAMPLES_RS, test: 'E_POOL' }] },
  E_CHANGED: { kind: 'not-a-load', reason: 'a sample file result', covered: [{ file: SAMPLES_RS, test: 'E_CHANGED' }] },
  E_EXISTS: { kind: 'not-a-load', reason: 'a dk_files_create result', covered: [{ file: 'modules/core/runtime/tests/file-write/file-write.device.test.ts', test: 'E_EXISTS' }] },
  E_CONTEXT: { kind: 'not-a-load', reason: 'a dk_files_* call outside an I/O step', covered: [{ file: 'modules/core/runtime/tests/file-write/file-write.device.test.ts', test: 'E_CONTEXT' }] },
  E_NO_REGISTRY: { kind: 'open', reason: 'needs an image without a registry' },
  E_REG_DEVICE: { kind: 'open', reason: 'needs a module whose register section names the other device' },
  E_TYPE_RANGE: { kind: 'host', reason: 'a patched record', covered: [{ file: CAP_RS, test: 'E_TYPE_RANGE' }] },
  E_TYPE_USED: { kind: 'host', reason: 'the registry hands out fresh types', covered: [{ file: COLLECT, test: 'E_TYPE_USED' }] },
  E_PARAM_FULL: { kind: 'host', reason: 'collect tests fill the rows', covered: [{ file: COLLECT, test: 'E_PARAM_FULL' }] },
  E_PAGE_FULL: { kind: 'host', reason: 'collect tests fill the rows', covered: [{ file: COLLECT, test: 'E_PAGE_FULL' }] },
  E_TYPE_IN_USE: { kind: 'reserved', reason: 'hot reload is archived; never returned' },
  E_DECL_ID: { fail: { file: COMMIT, test: 'a machine index that the keys table lacks is E_DECL_ID' }, pass: { file: COMMIT, test: 'the commit logs its rows' } },
  E_DECL_USES: { fail: { file: COMMIT, test: 'more rows than [uses] is E_DECL_USES' }, pass: { file: COMMIT, test: 'the commit logs its rows' } },
  E_FX_ID_DUP: { fail: { file: COMMIT, test: 'an effect id that a loaded module has is E_FX_ID_DUP' }, pass: { file: COMMIT, test: 'the commit logs its rows' } },
  E_DECL_FULL: { kind: 'host', reason: 'more builder calls than the record limit', covered: [{ file: COLLECT, test: 'E_DECL_FULL' }] },
  E_PLAN_KIND: { fail: mine('keys for a kind nobody owns are refused'), pass: loads },
  E_PLAN_OWNER: { fail: prov('two providers of one hook site are E_PLAN_OWNER'), pass: prov('a provider installs its hook and the consumer calls it') },
  E_PLAN_KEY: { fail: mine('a second owner of one key is E_PLAN_KEY'), pass: mine('the first owner of the key loads') },
  E_PLAN_FIT: { fail: mine('uses over the capacity are refused'), pass: loads },
  E_DECL_KIND: { fail: mine('kinds in the header table that init does not declare are E_DECL_KIND'), pass: loads },
  E_HOOK_STOCK: { fail: prov('a bound file whose stock bytes differ is E_HOOK_STOCK'), pass: prov('a provider installs its hook and the consumer calls it') },
  E_NO_DSP: { kind: 'host', reason: 'needs an image without the DSP loader', covered: [{ file: CLEANUP, test: 'E_NO_DSP' }] },
  E_DSP_FORMAT: { kind: 'host', reason: 'a malformed DSP section', covered: [{ file: CONTRACT, test: 'E_DSP_FORMAT' }] },
  E_DSP_IMPORT: { kind: 'open', reason: 'no fixture imports a DSP symbol the image lacks' },
  E_DSP_NOSPACE: { fail: { file: CAPACITY, test: 'full set' }, pass: { file: CAPACITY, test: 'full set' } },
  E_DSP_ROWS: { fail: { file: CAPACITY, test: 'full set' }, pass: { file: CAPACITY, test: 'full set' } },
  E_DSP_XFER: { kind: 'host', reason: 'a transfer that fails needs the DSP link to fail', covered: [{ file: CLEANUP, test: 'E_DSP_XFER' }] },
  E_DSP_DEVICE: { kind: 'host', reason: 'a DSP section for the other device', covered: [{ file: CONTRACT, test: 'E_DSP_DEVICE' }] },
  E_DSP_TYPE: { kind: 'host', reason: 'a voice type out of range or taken', covered: [{ file: COLLECT, test: 'E_DSP_TYPE' }] },
  E_SEL_PATH: { kind: 'not-a-load', reason: 'a SELECT result', covered: [{ file: 'modules/core/runtime/tests/storage/storage.device.test.ts', test: 'E_SEL_PATH' }] },
  E_SEL_COUNT: { kind: 'not-a-load', reason: 'a SELECT result', covered: [{ file: CAP_RS, test: 'E_SEL_COUNT' }] },
  E_SEL_WRITE: { kind: 'open', reason: 'the block write of the selection record fails: needs a card that refuses writes' },
  E_CARD_BUSY: { kind: 'open', reason: 'a card transfer in flight while the selection is saved' },
  E_SAFE_MODE: { fail: { file: SAFE, test: 'the safe-once flag gives safe mode' }, pass: { file: CAPACITY, test: 'files: a file of the staging buffer size' } },
  E_BOOT_ONLY: { kind: 'open', reason: 'a load call after the boot hook: no module or host command makes one' },
  E_RANGE: { kind: 'not-a-load', reason: 'a card write outside the store', covered: [{ file: 'modules/core/runtime/tests/card-safety.device.test.ts', test: 'E_RANGE' }] },
  E_CARD_RO: { kind: 'not-a-load', reason: 'the card area is not ours', covered: [{ file: 'modules/core/runtime/tests/card-safety.device.test.ts', test: 'E_CARD_RO' }] },
  E_CONTRACT: { fail: mine('a module that needs a newer runtime is E_CONTRACT'), pass: loads },
  E_TABLE: { kind: 'open', reason: 'the runtime tables fail their check: needs a corrupted table' },
  E_MISSING: { fail: mine('a hard need nobody provides is E_MISSING'), pass: loads },
  E_SVC_ABI: { fail: { file: LFO_DEVICE, test: 'another ABI' }, pass: { file: LFO_DEVICE, test: 'a fixture loads and unloads' } },
};
