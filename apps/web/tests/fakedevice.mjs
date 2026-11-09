// A fake digikit device that speaks the SysEx protocol of docs/design/digikit-upload.md byte for byte (framing included), as a `DkTransport` for
// `DkSession`: files, a selection, a log, modules, and a RESTART that is silent for a while and then boots again. Shared by dktransfer.test.mjs and
// the screenshot run of the site's Transfer page (tools/devtools/web_transfer_shots.mjs).
import { decode8in7, encode8in7, fileHash } from '@digi/dk-session';
import { CAP, CAPACITY_KINDS, CMD, LOG_KIND, SERVICE_CMD } from '@digi/dk-session/gen_proto';

/** The store's hash slot of a file (the host's `fileHash`: what the real runtime keeps in its table). */
export const sha = fileHash;
export const hex = (b) => Buffer.from(b).toString('hex');
export const concat = (...p) => Uint8Array.from(Buffer.concat(p.map((x) => Buffer.from(x))));
export const be32 = (n) => Uint8Array.of((n >>> 24) & 255, (n >>> 16) & 255, (n >>> 8) & 255, n & 255);
export const cs = (s) => concat(Buffer.from(s), Uint8Array.of(0));

export const ST = { E_STATE: -53, E_NOFILE: -59, E_HASH: -56, E_FULL: -52, E_OFFSET: -54 };

export class FakeDevice {
  constructor() {
    this.files = new Map();
    this.dirs = new Set(); // folders with nothing in them
    this.selection = [];
    this.seen = []; // the commands received, by name
    this.corrupt = false; // VERIFY answers a wrong hash
    this.pageSize = 2; // LIST entries per reply
    this.silent = 0; // requests to ignore (the reboot)
    this.loaded = []; // the names MODULES lists
    this.staged = null;
    this.log = [{ seq: 1, kind: LOG_KIND.BOOT_START, flags: 0, args: [516, 2, 0, 0, 0] }];
    this.queue = [];
    this.free = 1 << 20; // INFO's u32 free_bytes
    this.storeKib = null; // CAPACITY's STORE limit in KiB (null: the entry is left out); 1024 KiB are used
    this.caps = null; // INFO's capability word (null: every capability)
    this.imageId = null; // INFO's image id as 16 hex digits (null: the runtime before 0.5.2, which sends none)
    this.arenaLimit = 1000; // CAPACITY's ARENA and DSP_AREA limits (bytes)
    this.dspLimit = 1000;
    this.arenaLargest = 300; // the largest free block of each
    this.dspLargest = 50;
    this.os = null; // the OS version the stock MidiRpc SoftwareVersion answers (null: the request gets no answer)
    this.pending = 0; // after a RESTART: requests that are still answered by the old boot before the reset
  }
  reply(cmd, seq, status, body = new Uint8Array()) {
    const packed = encode8in7(concat(Uint8Array.of(status & 255), body));
    return concat([0xf0, 0x7d, 0x64, 0x6b, 1, cmd | 0x40, seq], packed, [0xf7]);
  }
  /** A stock MidiRpc request (`f0 00 20 3c 10 00`, 8-in-7 body `seq seq 00 00 type payload`): only SoftwareVersion (type 2) is answered. */
  rpc(msg) {
    const body = decode8in7(msg.subarray(6, msg.length - 1));
    if (body[4] !== 2 || this.os === null) return;
    const seq = body.subarray(0, 2);
    const strings = concat(...['00', '79', this.os].map((t) => cs(t)));
    this.queue.push(
      concat([0xf0, 0x00, 0x20, 0x3c, 0x10, 0x00], encode8in7(concat(seq, seq, [2 | 0x80], strings)), [0xf7]),
    );
  }
  send(msg) {
    if (msg[1] === 0x00) return this.rpc(msg);
    const cmd = msg[5];
    const seq = msg[6];
    const body = decode8in7(msg.subarray(7, msg.length - 1));
    if (this.silent > 0) {
      this.silent--;
      if (this.silent === 0) this.boot();
      return;
    }
    const name = Object.entries({ ...CMD, ...SERVICE_CMD }).find(([, v]) => v === cmd)?.[0] ?? `0x${cmd.toString(16)}`;
    this.seen.push(name);
    const r = this.handle(cmd, seq, body);
    if (r) this.queue.push(r);
    if (this.pending > 0 && cmd !== CMD.RESTART && --this.pending === 0) this.silent = 2; // the guest is resetting: two requests get no answer
  }
  boot() {
    this.log = [{ seq: 1, kind: LOG_KIND.BOOT_START, flags: 0, args: [516, 2, 0, 0, 0] }];
  }
  async recv() {
    return this.queue.shift() ?? null;
  }
  pathOf(body) {
    const end = body.indexOf(0);
    return [Buffer.from(body.subarray(0, end)).toString(), end + 1];
  }
  handle(cmd, seq, body) {
    const ok = (b) => this.reply(cmd, seq, 0, b);
    const err = (code, b) => this.reply(cmd, seq, code, b);
    switch (cmd) {
      case CMD.INFO: {
        const flags = this.staged ? 2 : 0;
        return ok(
          concat(
            Uint8Array.of(0, 1, 0x02, 0x04),
            cs('dt2'),
            be32(this.free),
            be32(32768),
            [flags],
            be32(1),
            be32(this.log.length),
            be32(0x000204),
            be32(this.caps ?? Object.values(CAP).reduce((a, bit) => a | (1 << bit), 0)),
            this.imageId ? Uint8Array.from(Buffer.from(this.imageId, 'hex')) : [],
          ),
        );
      }
      case CMD.LIST: {
        const first = new DataView(body.buffer, body.byteOffset).getUint32(0);
        const prefix = body.length > 4 ? Buffer.from(body.subarray(4, body.length - 1)).toString() : '';
        const all = [...this.files, ...[...this.dirs].map((d) => [d, new Uint8Array(0), true])]
          .filter(([p]) => p.startsWith(prefix))
          .sort(([a], [b]) => (a < b ? -1 : 1));
        const page = all.slice(first, first + this.pageSize);
        return ok(
          concat(
            be32(all.length),
            [page.length],
            ...page.map(([p, d, dir]) => concat(cs(p), be32(d.length), dir ? new Uint8Array(32) : sha(d))),
          ),
        );
      }
      case CMD.MKDIR: {
        const [path] = this.pathOf(body);
        if ([...this.files.keys()].some((f) => f === path || path.startsWith(`${f}/`))) return err(-51);
        const exists = [...this.dirs, ...this.files.keys()].some((f) => f === path || f.startsWith(`${path}/`));
        if (!exists) {
          for (const d of this.dirs) if (path.startsWith(`${d}/`)) this.dirs.delete(d);
          this.dirs.add(path);
        }
        return ok();
      }
      case CMD.BEGIN: {
        const [path, at] = this.pathOf(body);
        const size = new DataView(body.buffer, body.byteOffset).getUint32(at);
        this.staged = { path, size, sha: body.slice(at + 4, at + 36), data: new Uint8Array(0), last: -1 };
        return ok();
      }
      case CMD.DATA: {
        const st = this.staged;
        if (!st) return err(ST.E_STATE);
        const off = new DataView(body.buffer, body.byteOffset).getUint32(0);
        const part = body.subarray(4);
        if (off === st.last) return ok(); // the repeat of the last chunk
        if (off !== st.data.length) return err(ST.E_OFFSET);
        st.data = concat(st.data, part);
        st.last = off;
        return ok();
      }
      case CMD.END: {
        const st = this.staged;
        if (!st) return err(ST.E_STATE);
        this.staged = null;
        if (st.data.length !== st.size || hex(sha(st.data)) !== hex(st.sha)) return err(ST.E_HASH);
        for (const d of this.dirs) if (st.path.startsWith(`${d}/`)) this.dirs.delete(d);
        this.files.set(st.path, st.data);
        return ok();
      }
      case CMD.DELETE: {
        const [path] = this.pathOf(body);
        return this.files.delete(path) || this.dirs.delete(path) ? ok() : err(ST.E_NOFILE);
      }
      case CMD.READ: {
        const [path, at] = this.pathOf(body);
        const f = this.files.get(path);
        if (!f) return err(ST.E_NOFILE);
        const dv = new DataView(body.buffer, body.byteOffset);
        const off = dv.getUint32(at);
        const len = Math.min(dv.getUint32(at + 4), 768, f.length - off);
        return ok(concat(be32(f.length), f.subarray(off, off + len)));
      }
      case CMD.VERIFY: {
        const [path, at] = this.pathOf(body);
        const f = this.files.get(path);
        if (!f) return err(ST.E_NOFILE);
        if (body.length > at) {
          const dv = new DataView(body.buffer, body.byteOffset);
          const off = dv.getUint32(at);
          const len = dv.getUint32(at + 4);
          if (off % 512 !== 0 || off >= f.length) return err(ST.E_OFFSET);
          if (len === 0 || len > f.length - off) return err(-55);
          return ok(sha(f.subarray(off, off + len)));
        }
        return ok(this.corrupt ? sha(concat(f, [1])) : sha(f));
      }
      case CMD.SELECT: {
        const n = body[0];
        let at = 1;
        this.selection = [];
        for (let i = 0; i < n; i++) {
          const [p, used] = this.pathOf(body.subarray(at));
          this.selection.push(p);
          at += used;
        }
        return ok();
      }
      case CMD.RESTART:
        this.log.push({ seq: this.log.length + 1, kind: LOG_KIND.UPLOAD, flags: 0, args: [CMD.RESTART, 0, 0, 0, 0] });
        this.pending = 1;
        this.loaded = this.selection.map((p) => `mod:${p}`);
        return ok();
      case CMD.MODULES: {
        const entry = (name, i) => {
          const nm = new Uint8Array(32);
          nm.set(Buffer.from(name));
          return concat(
            [i],
            nm,
            be32(0x10000),
            [1, 1, 1, 0, 0, 0, 1, 0],
            be32(0x47e8b000),
            be32(256),
            be32(0),
            be32(0),
            be32(1),
            [1, 17],
            be32(0),
            [0],
          );
        };
        return ok(
          concat(
            be32(100),
            be32(90),
            be32(80),
            be32(70),
            [this.loaded.length, this.loaded.length],
            ...this.loaded.map(entry),
          ),
        );
      }
      case CMD.CAPACITY: {
        const store = this.storeKib === null ? [] : [[CAPACITY_KINDS.STORE], be32(this.storeKib), be32(1024), be32(0)];
        return ok(
          concat(
            [1, 3 + (this.storeKib === null ? 0 : 1)],
            [CAPACITY_KINDS.MODULES],
            be32(8),
            be32(this.loaded.length),
            be32(0),
            [CAPACITY_KINDS.ARENA],
            be32(this.arenaLimit),
            be32(0),
            be32(this.arenaLargest),
            [CAPACITY_KINDS.DSP_AREA],
            be32(this.dspLimit),
            be32(0),
            be32(this.dspLargest),
            ...store,
          ),
        );
      }
      case SERVICE_CMD.LOG: {
        const from = new DataView(body.buffer, body.byteOffset).getUint32(0);
        const ev = this.log.filter((e) => e.seq >= from).slice(0, 3);
        const out = ev.map((e) =>
          concat(be32(e.seq), be32(0), [e.kind >> 8, e.kind & 255, e.flags >> 8, e.flags & 255], ...e.args.map(be32)),
        );
        return ok(concat(be32(1), be32(this.log.length), [ev.length], ...out));
      }
      default:
        return err(-60);
    }
  }
}
