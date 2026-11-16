// The Transfer commands of wb (tools/devtools/wb.mjs): the Transfer pane's install and delete, through the pane's own code
// (packages/workbench-core/src/modules/dktransfer.ts). `install` resolves the services each module needs, binds them to your stock firmware and
// uploads them before the modules; `delete` takes the services no remaining module needs along with the modules. No child process.
//
//     wb [--port SUB] [--bin PATH] transfer install ID... [--stock STOCK.syx]
//     wb [--port SUB] [--bin PATH] transfer delete PATH...
//
// ID is a module id of the library (`out/modules/<device>/`, `dist/modules/<device>/` for the `.dks` services); PATH is a card path (`modules/pluck.dkm`).
// `--stock` is the stock firmware .syx of the device: it is what a service that hooks the firmware is bound to. Without it a module that needs such a
// service is refused (the others install). One JSON object per event, like the pane's progress (`--format text` prints the pane's lines).
import { createHash } from 'node:crypto';
import { readFileSync } from 'node:fs';
import { fileHashHex } from '../../packages/dk-session/src/index.ts';
import { DkNeeds } from '../../packages/workbench-core/src/modules/dkneeds.ts';
import { DkBind, createServiceBinder } from '../../packages/workbench-core/src/modules/dkbind.ts';
import { RUNTIME_SERVICE_NAMES } from '../../packages/dk-session/src/gen_proto.ts';
import {
  SERVICE_EXT,
  deleteFiles,
  install,
  libraryModule,
} from '../../packages/workbench-core/src/modules/dktransfer.ts';
import { nameHash, serviceFileOf } from '../../packages/workbench-core/src/modules/services.ts';
import { eventText } from '../../packages/workbench-core/src/transfer/transfer-core.ts';

/** The library of a device: the built modules and the unbound services (the files the Transfer pane lists). */
async function loadLibrary(host, device, needs) {
  const modules = [];
  const services = [];
  for (const f of await host.library.scan(device)) {
    const bytes = new Uint8Array(await host.library.read(device, f.path));
    if (f.path.endsWith(SERVICE_EXT)) {
      const stem = (f.path.split('/').pop() ?? f.path).slice(0, -SERVICE_EXT.length);
      const s = serviceFileOf(stem, bytes, fileHashHex(bytes));
      if (s) services.push(s);
    } else {
      const m = await libraryModule(bytes, (b) => needs.of(b));
      if (m) modules.push(m);
    }
  }
  return { modules, services };
}

export async function transferCommand({ host, session, sub, rest, opts, fail }) {
  const emit = (e) => console.log(opts.format === 'text' ? eventText(e) : JSON.stringify(e));
  if (sub === 'delete') {
    if (rest.length === 0) fail('transfer delete needs card paths');
    await deleteFiles(session, rest, emit);
    return 0;
  }
  if (sub !== 'install' || rest.length === 0)
    fail('usage: wb transfer install ID... [--stock STOCK.syx] | wb transfer delete PATH...');
  const info = await session.info();
  const wasm = new Uint8Array(await host.firmware.patchkitWasm());
  const needs = await DkNeeds.create(wasm);
  const { modules, services } = await loadLibrary(host, info.port, needs);
  const stock = opts.stock ? new Uint8Array(readFileSync(opts.stock)) : undefined;
  const stockHash = stock ? createHash('sha256').update(stock).digest('hex') : null;
  const binder = createServiceBinder(
    async () => DkBind.create(wasm, new Uint8Array(await host.firmware.bindBundle())),
    { bytesOf: async (h) => (h === stockHash ? stock : undefined) },
  );
  const deps = {
    services,
    binder,
    firmwareHash: stockHash,
    runtimeIdentity: (i) => ({ version: i.runtime, fingerprint: i.svc_abi }),
    runtimeServices: new Set(RUNTIME_SERVICE_NAMES.map(nameHash)),
    cache: new Map(),
    needsOf: (b) => needs.of(b),
  };
  const r = await install(session, rest, modules, emit, deps);
  return r.refused?.length && r.uploaded.length === 0 && rest.length === r.refused.length ? 1 : 0;
}
