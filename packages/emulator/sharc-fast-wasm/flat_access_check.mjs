// Execute real Lower-produced kernels exported by flat_access_wasm32_fixture:
//   WASM_FLAT_OUT=IR_DIR cargo test -p sharc-native --lib flat_access_wasm32_fixture
//   cargo run -p sharc-fast-wasm --bin wasmgen -- IR_DIR WASM_DIR
//   node packages/emulator/sharc-fast-wasm/flat_access_check.mjs IR_DIR WASM_DIR [RESULT_JSON]
// The Rust fixture simulates wasm32 pointer layout on the host. This checks
// emitted backend exits, not a target-native build, Mem policy lifecycle,
// semantic fallback, browser integration, or the diagnostic RPC.
import fs from 'node:fs';
import path from 'node:path';
import assert from 'node:assert/strict';

const [irDir, wasmDir, resultPath] = process.argv.slice(2);
assert.ok(irDir && wasmDir, 'expected IR_DIR WASM_DIR [RESULT_JSON]');
const layout = JSON.parse(fs.readFileSync(path.join(irDir, 'layout.json'), 'utf8'));
assert.equal(layout.layout, 'simulated_wasm32');
assert.equal(layout.pointer_bytes, 4);
assert.equal(layout.page_bits, 16);
assert.equal(layout.page_size, 1 << layout.page_bits);
assert.equal(layout.page_present_off, layout.page_size);
assert.equal(layout.page_dirty_off, layout.page_size + layout.page_size / 8);
assert.equal(layout.kernels.length, 6);
const ctx = 0x18000;
const eff = 0x30000;
const watch = 0x70000;
const page = 0x80000;
const audio = 0xa0000;
const guestPage = 0x80010000;
const index = guestPage >>> layout.page_bits;
const c = layout.ctx;
const sentinel = 0xdeadbeef;
const results = [];
assert.ok(ctx >= layout.page_dirty_off + layout.page_size / 8);
assert.ok(ctx + c.size <= eff);
assert.equal(eff + 65536 * layout.pointer_bytes, watch);
assert.equal(watch + 65536, page);
assert.ok(page + layout.page_dirty_off + layout.page_size / 8 <= audio);

for (const spec of layout.kernels) {
  assert.ok([1, 2, 4].includes(spec.bytes));
  assert.equal(typeof spec.write, 'boolean');
  const module = new WebAssembly.Module(fs.readFileSync(path.join(wasmDir, `${spec.name}.wasm`)));
  assert.deepEqual(WebAssembly.Module.imports(module), [{ module: 'env', name: 'memory', kind: 'memory' }]);
  const memory = new WebAssembly.Memory({ initial: 16 });
  const instance = new WebAssembly.Instance(module, { env: { memory } });
  const bytes = new Uint8Array(memory.buffer);
  const view = new DataView(memory.buffer);
  const kernel = instance.exports.k0;

  const runCase = (name, offset, ok, options = {}) => {
    bytes.fill(0x6d);
    bytes.fill(0, eff, watch + 65536);
    // Both null-address storage and the real page have valid bitmap bits.
    // Without the null guard the refused access would succeed against low
    // memory, rather than getting rejected by an unrelated later bitmap test.
    for (const base of [0, page]) {
      bytes.fill(0xff, base + layout.page_present_off, base + layout.page_dirty_off + layout.page_size / 8);
    }
    bytes.fill(0xa6, audio, audio + 4096); // An unrelated audio buffer must stay intact.
    for (let i = 0; i < 4 && offset + i < layout.page_size; i++) {
      bytes[page + offset + i] = [0x83, 0x17, 0xe9, 0x46][i];
    }
    if (options.sameValue) {
      for (let i = 0; i < spec.bytes; i++) bytes[page + offset + i] = (layout.store_value >>> (8 * i)) & 0xff;
    }
    const clearBit = (map, bit) => {
      bytes[page + map + (bit >>> 3)] &= ~(1 << (bit & 7));
    };
    if (options.missing)
      clearBit(spec.write ? layout.page_dirty_off : layout.page_present_off, offset + spec.bytes - 1);
    view.setUint32(eff + index * layout.pointer_bytes, options.nullPointer ? 0 : page, true);
    bytes[watch + index] = options.codeWatch ? 1 : 0;
    bytes.fill(0xcc, ctx, ctx + c.size);
    view.setUint32(ctx + c.iters, 1, true);
    view.setUint32(ctx + c.done, sentinel, true);
    view.setUint32(ctx + c.exit_k, sentinel, true);
    view.setUint32(ctx + c.regs_in, guestPage + offset, true);
    view.setUint32(ctx + c.regs_out, sentinel, true);
    // Context slots are fixed u64 on both targets, even with 32-bit entries.
    view.setBigUint64(ctx + c.flat_eff, BigInt(eff), true);
    view.setBigUint64(ctx + c.flat_watch, BigInt(watch), true);
    const expected = bytes.slice();
    const expectedView = new DataView(expected.buffer);
    expectedView.setUint32(ctx + c.done, ok ? 1 : 0, true);
    if (!ok) expectedView.setUint32(ctx + c.exit_k, layout.exit_k, true);
    if (ok && spec.write) {
      for (let i = 0; i < spec.bytes; i++) expected[page + offset + i] = (layout.store_value >>> (8 * i)) & 0xff;
    } else if (ok) {
      let word = 0;
      for (let i = 0; i < spec.bytes; i++) word = (word | (bytes[page + offset + i] << (8 * i))) >>> 0;
      expectedView.setUint32(ctx + c.regs_out, word, true);
    }
    // Reuse the same compiled instance as the table entry changes between
    // normal, null and restored cases. A refusal must be a guard exit, not a trap.
    assert.equal(kernel(ctx, 0), ok ? 0 : 1, `${spec.name}: ${name}: return`);
    assert.deepEqual(bytes, expected, `${spec.name}: ${name}: all memory, including audio and null storage`);
    results.push({ kernel: spec.name, case: name, result: ok ? 'completed' : 'guard_exit' });
  };

  runCase('normal', 0x100, true);
  runCase('watched-null-pointer', 0x100, false, { nullPointer: true });
  runCase('restored-pointer', 0x100, true);
  runCase('page-end-valid', layout.page_size - spec.bytes, true);
  runCase('missing-bitmap-bit', 0x100, false, { missing: true });
  if (spec.write) {
    runCase('same-value-store', 0x100, true, { sameValue: true });
    runCase('code-watch', 0x100, false, { codeWatch: true });
    runCase('bitmap-end-valid', 32 - spec.bytes, true);
  }
  if (spec.bytes > 1) {
    runCase('page-crossing', layout.page_size - 1, false);
    runCase('bitmap-crossing', 31, !spec.write);
    if (!spec.write) runCase('missing-bitmap-tail', 31, false, { missing: true });
  }
}
const result = {
  passed: true,
  layout: layout.layout,
  kernels: layout.kernels.length,
  cases: results.length,
  engine: { node: process.version, v8: process.versions.v8 },
  scope:
    'Actual Lower output with simulated wasm32 pointer layout, emitted as Wasm and executed in V8. Safe backend exits only; full-engine fallback and browser RPC unproven.',
  results,
};
if (resultPath) fs.writeFileSync(resultPath, `${JSON.stringify(result, null, 2)}\n`);
console.log(
  `flat_access_check: ${result.kernels} real lowered kernels, ${result.cases} cases passed (${process.version})`,
);
