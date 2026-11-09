/** Validation shared only by the private observer's native fixture and host regressions. */
export type StorageWatchEvent = {
  t: string;
  ic: number;
  pc?: string;
  addr?: string;
  size?: number;
  value?: string;
};
export type StorageWatchCode = [number, number];

export function normalizeStorageWatchAddress(address: number): number {
  return address >= 0x48000000 && address < 0x50000000 ? address - 0x08000000 : address;
}

export function validateStorageWatch(
  rows: StorageWatchEvent[],
  summary: { events: number; logged: number },
  latch: number,
  mask: number,
  allowedCode: StorageWatchCode[],
): (StorageWatchEvent & { address: number })[] {
  if (!Number.isSafeInteger(summary.logged) || summary.logged < 0 || summary.logged !== summary.events)
    throw new Error('storage watch report is truncated or invalid');
  // Other informational rows (for example CACR) do not increment report/logged.
  // The fixture disables stack/flush options; report() counts w/dma rows only.
  const writes = rows.filter((row) => row.t === 'w' || row.t === 'dma');
  if (writes.length !== summary.logged) throw new Error('storage watch parsed write count differs from logged count');
  return writes.map((row) => {
    const raw = Number(row.addr);
    if (!Number.isSafeInteger(raw) || raw < 0 || raw > 0xffffffff || !Number.isInteger(row.size) || row.size! <= 0)
      throw new Error('invalid storage watch write address/size');
    const address = normalizeStorageWatchAddress(raw);
    if (address < latch + 4 && address + row.size! > latch) {
      if (row.t !== 'w') throw new Error('non-CPU latch writer');
      const pc = Number(row.pc);
      if (!allowedCode.some(([lo, hi]) => lo <= pc && pc < hi)) throw new Error('unexpected latch writer');
      if (address !== latch || row.size !== 4) throw new Error('unexpected latch write footprint');
      const value = Number(row.value);
      if (!Number.isSafeInteger(value) || value < 0 || value > 0xffffffff || (value & ~mask) >>> 0 !== 0)
        throw new Error('invalid latch write value');
    }
    return { ...row, address };
  });
}
