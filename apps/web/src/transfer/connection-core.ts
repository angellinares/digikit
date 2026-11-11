// The connection indicator of the Transfer pane (pure; apps/web/tests/transfer-connection.test.ts). The state follows the session of the
// selected target: an INFO that was answered is "connected"; a request that fails after its retries (no reply, a closed port, a paused
// emulator) or a port that has gone is "not connected"; a request on its way, or a new target, is "connecting". A device that answers with an
// error status is still there, and a cancel by the user says nothing about the device.
import { isCancelled, isNoRuntime, isTimeout } from '../modules/backend.ts';

/** What INFO says about the device (the runtime has no serial number and no OS version; see docs/design/digikit-upload.md, INFO). */
export interface DeviceFacts {
  /** The device id INFO reports (`dt2`, `dn2`): the most specific id there is. */
  id: string;
  /** The profile's name of the device (`Digitakt II`). */
  name: string;
  /** The firmware version, when the app knows it (INFO does not carry it). */
  os?: string;
  /** The runtime version, `0.5.2`; left out for a device that has no runtime (the stock firmware: the Firmware page). */
  runtime?: string;
}

export type Connection =
  | { state: 'connecting' }
  | { state: 'connected'; device: DeviceFacts }
  | { state: 'noruntime'; reason: string; os?: string }
  | { state: 'disconnected'; reason: string };

export type ConnectionEvent =
  | { type: 'target' }
  | { type: 'asking' }
  | { type: 'answered'; device: DeviceFacts }
  | { type: 'noruntime'; reason: string; os?: string }
  | { type: 'lost'; reason: string }
  | { type: 'cancelled' }
  | { type: 'closed'; reason?: string };

export const CONNECTING: Connection = { state: 'connecting' };

export function step(c: Connection, e: ConnectionEvent): Connection {
  switch (e.type) {
    case 'target':
      return CONNECTING;
    case 'asking':
      // a read does not flicker the indicator
      return c.state === 'connected' || c.state === 'noruntime' ? c : CONNECTING;
    case 'answered':
      return { state: 'connected', device: e.device };
    case 'noruntime':
      return { state: 'noruntime', reason: e.reason, ...(e.os ? { os: e.os } : {}) };
    case 'lost':
      return { state: 'disconnected', reason: e.reason };
    case 'cancelled':
      // the user gave up the wait: a device that had answered before is still there
      return c.state === 'connecting' ? { state: 'disconnected', reason: 'Cancelled before the device answered.' } : c;
    case 'closed':
      return { state: 'disconnected', reason: e.reason ?? 'The MIDI port is gone.' };
  }
}

/** Does this error mean the device cannot be reached (and not: the user cancelled, or the device answered with an error)? */
export function isLost(e: unknown): boolean {
  if (isCancelled(e)) return false;
  if (isNoRuntime(e)) return false; // it answered
  if (isTimeout(e)) return true;
  const kind = (e as { kind?: string } | null)?.kind;
  return !(kind === 'device' || kind === 'protocol' || kind === 'request');
}

/** The first line of an error, for the indicator's tooltip. */
export function lostReason(e: unknown): string {
  const raw = e instanceof Error ? e.message : String(e);
  return raw.split('\n')[0] ?? raw;
}

/** The stock MidiRpc type of SoftwareVersion (docs/MIDI-SYSEX-RPC.md): no payload; the reply holds NUL-terminated strings, one of them the OS version. */
export const RPC_SOFTWARE_VERSION = 0x02;

/** The OS version in the reply of SoftwareVersion (`"00"`, `"79"`, `"1.17"`: the string that looks like a version), or undefined. */
export function parseSoftwareVersion(payload: Uint8Array): string | undefined {
  const text = new TextDecoder('latin1').decode(payload);
  return text.split('\0').find((s) => /^\d+\.\d+[0-9A-Za-z.]*$/.test(s));
}

/** The id as shown: a long id is cut (the tooltip has all of it). */
export function shortId(id: string): string {
  return id.length > 12 ? `${id.slice(0, 8)}…` : id;
}

export interface ConnectionView {
  state: Connection['state'];
  /** "Connected", "Connecting…", "Not connected". */
  label: string;
  /** The details after the label, one part each. */
  parts: string[];
  /** All of it, for the tooltip. */
  title: string;
}

/** The text of the indicator; `port` is the name of the target (`MIDI: Elektron Digitakt II`, `Emulator`). */
export function connectionView(c: Connection, port: string): ConnectionView {
  if (c.state === 'connected') {
    const d = c.device;
    const os = d.os ? [`OS ${d.os}`] : [];
    const parts = [
      d.name,
      ...os,
      ...(d.runtime ? [`runtime ${d.runtime}`, `id ${shortId(d.id)}`] : ['no runtime']),
      port,
    ];
    const full = [d.name, ...os, ...(d.runtime ? [`runtime ${d.runtime}`, `id ${d.id}`] : ['no runtime']), port];
    return { state: c.state, label: 'Connected', parts, title: full.join(' · ') };
  }
  if (c.state === 'connecting') {
    return { state: c.state, label: 'Connecting…', parts: [port], title: `Asking ${port}` };
  }
  if (c.state === 'noruntime') {
    return {
      state: c.state,
      label: 'No runtime',
      parts: [...(c.os ? [`OS ${c.os}`] : []), port],
      title: `${port}: ${c.reason}`,
    };
  }
  return { state: c.state, label: 'Not connected', parts: [port], title: `${port}: ${c.reason}` };
}
