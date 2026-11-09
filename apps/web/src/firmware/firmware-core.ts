// The Firmware page's rules, without the screen: what the checks are called, when Flash is allowed, how a device is matched with an image,
// how the progress reads, and what the device's answer after the restart means. Pure; apps/web/tests/firmware-core.test.ts. The page is
// FirmwarePage.tsx, the run itself (sending, waiting for the restart) is firmware-flow.ts.
import type { PreparedFlash } from '../patch/patchkit-core';

export type DeviceId = 'dt2' | 'dn2';

export const DEVICE_NAMES: Record<DeviceId, string> = { dt2: 'Digitakt II', dn2: 'Digitone II' };

/** Which device a MIDI port name belongs to (a stock device has no INFO to say it). */
export function modelOfPort(port: string): DeviceId | undefined {
  if (/digitakt/i.test(port)) return 'dt2';
  if (/digitone/i.test(port)) return 'dn2';
  return undefined;
}

export const isDeviceId = (d: string): d is DeviceId => d === 'dt2' || d === 'dn2';

// ---- the image and its checks --------------------------------------------------------------------------------------------------

/** The file the user picked and what patchkit made of it. */
export type PrepareState =
  | { kind: 'none' }
  | { kind: 'working'; name: string }
  | { kind: 'error'; name: string; message: string }
  | { kind: 'ready'; name: string; size: number; p: PreparedFlash };

/** How the checks of `flash::prepare` are named on the page; a name that is not here is shown as it is. */
const CHECK_LABELS: Record<string, string> = {
  stock: 'Stock image',
  patch: 'Patched',
  preflight: 'Bootloader and updater unchanged',
  verify: 'Checksums, HMAC and size',
  product: 'Right device',
  'build floor': 'Build number',
  'DSP scan': 'DSP scan',
  'round trip': 'Stream round trip',
  'image id': 'Image id',
};

export interface CheckRow {
  name: string;
  label: string;
  ok: boolean;
  detail: string;
}

/** The rows of the Checks step; the runtime version joins the label of "Patched". */
export function checkRows(p: PreparedFlash): CheckRow[] {
  return p.checks.map((c) => {
    let label = CHECK_LABELS[c.name] ?? c.name;
    if (c.name === 'patch' && p.runtime) label = `Patched (runtime ${p.runtime})`;
    return { name: c.name, label, ok: c.ok, detail: c.detail };
  });
}

/** What the page says of a file patchkit refused: its own text, with a lead-in when it does not say which file it was. */
export function refusalText(message: string): string {
  const m = message.trim();
  if (/^this file/i.test(m)) return m.charAt(0).toUpperCase() + m.slice(1);
  return `This is not an Elektron firmware update file: ${m}`;
}

/** The first 4 digits of a hash, for the file line. */
export const shortSha = (sha: string): string => `${sha.slice(0, 4)}…`;

/** The .syx file name of the image that can be downloaded. */
export function imageFileName(p: PreparedFlash): string {
  const name = DEVICE_NAMES[p.device as DeviceId] ?? p.device;
  return `${name.replace(/\s+/g, '_')}_OS${p.os}${p.restore ? '' : `+digikit-${p.runtime ?? 'runtime'}`}.syx`;
}

// ---- the device ----------------------------------------------------------------------------------------------------------------

/** What is known of the device on the chosen port. */
export interface DeviceFacts {
  /** The port name. */
  port: string;
  /** The model, from INFO or else from the port name. */
  model: DeviceId | undefined;
  /** The OS version the stock firmware reports (SoftwareVersion), when it answered. */
  os?: string;
  /** The runtime version, when the digikit runtime answered INFO. */
  runtime?: string;
  /** The id of the flashed image INFO reports. */
  imageId?: string | null;
}

export type DeviceState =
  | { kind: 'none'; why: string }
  | { kind: 'checking' }
  | { kind: 'connected'; d: DeviceFacts }
  | { kind: 'lost'; reason: string };

export interface MatchView {
  /** `match`: the model is the image's; `mismatch`: another model; `unknown`: no device or no model; */
  state: 'none' | 'match' | 'mismatch' | 'unknown';
  /** One line. */
  text: string;
  /** The OS lines: what runs and what the image installs. */
  os?: string;
}

/** Does the connected device take this image? The OS version of the image is not compared: the image replaces the OS. */
export function matchView(p: PreparedFlash | undefined, dev: DeviceState): MatchView {
  if (dev.kind !== 'connected') return { state: 'none', text: '' };
  const d = dev.d;
  if (!p) return { state: 'unknown', text: '' };
  const want = DEVICE_NAMES[p.device as DeviceId] ?? p.device;
  if (!d.model) {
    return {
      state: 'unknown',
      text: `The port name does not say which device this is. The image is for a ${want}.`,
    };
  }
  const have = DEVICE_NAMES[d.model];
  if (d.model !== p.device) {
    return { state: 'mismatch', text: `This is a ${have}. The image is for a ${want}: it would not start.` };
  }
  const os = d.os ? `The device runs OS ${d.os}. ` : '';
  const installs = `The image installs OS ${p.os}${p.restore ? ' (stock)' : ` with digikit runtime ${p.runtime ?? ''}`.trimEnd()}.`;
  return { state: 'match', text: 'matches the image', os: `${os}${installs}` };
}

// ---- the run -------------------------------------------------------------------------------------------------------------------

export type FailCode =
  'send' | 'no-restart' | 'no-return' | 'no-answer' | 'runtime' | 'image-id' | 'os' | 'still-runtime' | 'not-sent';

/** How the image gets to the device: `rpc` is the MidiRpc route (the device acknowledges every Write, the page answers the device's own check at once); `menu` is the old
 * route into the device's OS UPGRADE screen (no reply, paced). */
export type FlashRoute = 'rpc' | 'menu';

export type FlashState =
  | { phase: 'idle' }
  // `unit: 'bytes'` (rpc): `sent` and `total` are bytes the device has taken; otherwise messages. `checking`: the last Write is in, the device checks the image. `flashing`: the device accepted it and the page committed; it cannot be cancelled now.
  | {
      phase: 'sending';
      sent: number;
      total: number;
      startedMs: number;
      nowMs: number;
      unit?: 'bytes';
      checking?: boolean;
      flashing?: boolean;
    }
  | { phase: 'stopped'; reason: 'cancelled' | 'error'; sent: number; total: number; message: string; unit?: 'bytes' }
  | { phase: 'rebooting'; stage: 'leaving' | 'returning' | 'asking'; sinceMs: number; nowMs: number; rpc?: true }
  | { phase: 'done'; message: string; lines: string[] }
  | { phase: 'failed'; code: FailCode; message: string };

export const IDLE: FlashState = { phase: 'idle' };

/** A run is in progress (Cancel is shown; nothing else may start). */
export const running = (s: FlashState): boolean => s.phase === 'sending' || s.phase === 'rebooting';

const THIN = ' ';
/** 9812 as "9 812" (a thin space). */
export const groups = (n: number): string =>
  Math.round(n)
    .toString()
    .replace(/\B(?=(\d{3})+(?!\d))/g, THIN);

/** "18 s left", "2 min 5 s left"; nothing before the rate is known. */
export function timeLeft(sent: number, total: number, startedMs: number, nowMs: number): string {
  const elapsed = nowMs - startedMs;
  if (sent < 20 || elapsed < 500) return '';
  const s = Math.max(0, Math.round(((total - sent) * (elapsed / sent)) / 1000));
  return s >= 60 ? `${Math.floor(s / 60)} min ${s % 60} s left` : `${s} s left`;
}

export interface ProgressView {
  /** 0 to 1 */
  fraction: number;
  text: string;
}

/** 1531152 as "1.53 MB" (decimal megabytes, as file sizes are shown). */
export const fmtMB = (n: number): string => `${(n / 1e6).toFixed(2)} MB`;

export function progressView(s: Extract<FlashState, { phase: 'sending' }>): ProgressView {
  if (s.unit === 'bytes') {
    const left = timeLeft(s.sent, s.total, s.startedMs, s.nowMs);
    return {
      fraction: s.total ? s.sent / s.total : 0,
      text: s.flashing
        ? 'the device checked the image ✓ · flashing, do not unplug'
        : s.checking
          ? 'the device is checking the image…'
          : [`sending ${(s.sent / 1e6).toFixed(2)} of ${fmtMB(s.total)}`, left].filter(Boolean).join(' · '),
    };
  }
  const left = timeLeft(s.sent, s.total, s.startedMs, s.nowMs);
  return {
    fraction: s.total ? s.sent / s.total : 0,
    text: [`${groups(s.sent)} / ${groups(s.total)}`, left].filter(Boolean).join(' · '),
  };
}

/** What the page says while the device restarts. */
export function rebootText(s: Extract<FlashState, { phase: 'rebooting' }>): string {
  const secs = Math.max(0, Math.round((s.nowMs - s.sinceMs) / 1000));
  switch (s.stage) {
    case 'leaving':
      if (s.rpc) return `The device erases and writes its flash (${secs} s). Do not touch it or the cable.`;
      return `All messages sent. The device checks the image and writes its flash (${secs} s). Do not touch it or the cable.`;
    case 'returning':
      return `The device is restarting (${secs} s). Keep the cable in.`;
    case 'asking':
      return `The device is back. Asking it which image it runs (${secs} s).`;
  }
}

export const CANCEL_TEXT =
  'Sending stopped. The device still shows RECEIVING and has changed nothing. Press NO on the device to cancel, or send again from the start.';

export const sendFailedText = (index: number, total: number, why: string): string =>
  `Message ${groups(index + 1)} of ${groups(total)} failed: ${why}. The device has changed nothing and still shows RECEIVING. Press NO on the device, or send again from the start.`;

/** What the device told us after the restart. */
export interface Answer {
  /** The OS version of SoftwareVersion (stock firmware and runtime both answer it). */
  os?: string;
  /** The runtime version INFO reports; undefined when INFO did not answer. */
  runtime?: string;
  imageId?: string | null;
}

export type Verdict = { ok: true; message: string; lines: string[] } | { ok: false; code: FailCode; message: string };

/**
 * The end of a patch: the device reports the runtime of the patch and the id of the image `prepare` made. For a restore: the stock OS,
 * and no runtime.
 */
export function judge(p: PreparedFlash, a: Answer): Verdict {
  const name = DEVICE_NAMES[p.device as DeviceId] ?? p.device;
  if (p.restore) {
    if (a.runtime) {
      return {
        ok: false,
        code: 'still-runtime',
        message: `The device still runs the digikit runtime ${a.runtime}. The stock image did not take effect: look at the device screen, then send it again.`,
      };
    }
    if (a.os !== p.os) {
      return {
        ok: false,
        code: 'os',
        message: `The device reports OS ${a.os ?? 'nothing'}, the stock image is OS ${p.os}. Look at the device screen.`,
      };
    }
    return {
      ok: true,
      message: `Restarted · stock OS ${p.os}, no runtime`,
      lines: [`${name} reports OS ${a.os}`, 'No digikit runtime answers.'],
    };
  }
  if (a.runtime !== p.runtime) {
    return {
      ok: false,
      code: 'runtime',
      message: `The device reports runtime ${a.runtime ?? 'none'}, the patch has ${p.runtime}. The new image may not be running: look at the device screen.`,
    };
  }
  if ((a.imageId ?? null) !== p.imageId) {
    return {
      ok: false,
      code: 'image-id',
      message: `The image id differs: the device reports ${a.imageId ?? 'none'}, the patch made ${p.imageId}. The device may run another image: look at the device screen.`,
    };
  }
  return {
    ok: true,
    message: `Restarted · runtime ${a.runtime} · image id ${a.imageId} matches`,
    lines: [`${name} OS ${a.os ?? p.os}`, `Runtime ${a.runtime}`, `Image id ${a.imageId}`],
  };
}

export const noReturnText = (device: string): string =>
  `The device did not answer on any port named ${device}. Wait a little, then look at its screen and the USB cable. If it does not start, use the recovery steps.`;
export const STOPPED_WAITING_TEXT = 'You stopped waiting. The device carries on by itself.';

/** Is `name` the port of `device`? CoreMIDI adds a number when it names a second port alike ("Digitakt II 2"). */
export const samePortName = (device: string, name: string): boolean =>
  name === device || (name.startsWith(`${device} `) && /^\d+$/.test(name.slice(device.length + 1)));
export const NO_ANSWER_TEXT =
  'The device is back but does not answer. The new image may not have reached the runtime: look at its screen. If it does not start, use the recovery steps.';

// ---- when Flash is allowed ----------------------------------------------------------------------------------------------------

export interface FlashInputs {
  prepare: PrepareState;
  device: DeviceState;
  /** The box "The device shows WAITING FOR SYSEX" (menu route only). */
  ack: boolean;
  run: FlashState;
  route: FlashRoute;
}

/** Flash is on when every check passed, the device is the image's model, and the box is ticked; otherwise the reason, in one line. */
export function canFlash(i: FlashInputs): { ok: boolean; reason: string } {
  if (running(i.run)) return { ok: false, reason: 'A flash is running.' };
  if (i.prepare.kind === 'none') return { ok: false, reason: 'Choose the stock firmware file first.' };
  if (i.prepare.kind === 'working') return { ok: false, reason: 'The file is being checked.' };
  if (i.prepare.kind === 'error') return { ok: false, reason: 'This file cannot be used.' };
  const p = i.prepare.p;
  if (!p.ok) return { ok: false, reason: 'A check failed: nothing is sent.' };
  if (i.device.kind === 'none') return { ok: false, reason: i.device.why };
  if (i.device.kind === 'checking') return { ok: false, reason: 'Looking for the device…' };
  if (i.device.kind === 'lost') return { ok: false, reason: 'The device is not connected.' };
  const m = matchView(p, i.device);
  if (m.state !== 'match') return { ok: false, reason: m.text };
  if (i.route === 'menu' && !i.ack)
    return { ok: false, reason: 'Tick the box when the device shows WAITING FOR SYSEX.' };
  return { ok: true, reason: '' };
}

/** The state of each step for its marker: done (lit), the one to do now, or later. */
export type StepMark = 'done' | 'now' | 'later' | 'bad';

export function stepMarks(i: FlashInputs): StepMark[] {
  return i.route === 'rpc' ? stepMarksRpc(i) : stepMarksMenu(i);
}

/** The RPC route has six steps: file, checks, device, prepare, flash, done. */
function stepMarksRpc(i: FlashInputs): StepMark[] {
  if (i.run.phase === 'done') return Array<StepMark>(6).fill('done');
  const ready = i.prepare.kind === 'ready' ? i.prepare.p : undefined;
  const checksOk = ready?.ok === true;
  const match = matchView(ready, i.device).state === 'match';
  const r = i.run;
  const sendMark: StepMark = !match
    ? 'later'
    : r.phase === 'rebooting' || r.phase === 'failed'
      ? 'done'
      : r.phase === 'stopped' && r.reason === 'error'
        ? 'bad'
        : 'now';
  return [
    ready ? 'done' : i.prepare.kind === 'error' ? 'bad' : 'now',
    !ready ? 'later' : checksOk ? 'done' : 'bad',
    !checksOk ? 'later' : match ? 'done' : i.device.kind === 'connected' ? 'bad' : 'now',
    !match ? 'later' : 'done',
    sendMark,
    r.phase === 'rebooting' ? 'now' : r.phase === 'failed' ? 'bad' : 'later',
  ];
}

function stepMarksMenu(i: FlashInputs): StepMark[] {
  // a finished run: every step was done
  if (i.run.phase === 'done') return ['done', 'done', 'done', 'done', 'done', 'done'];
  const ready = i.prepare.kind === 'ready' ? i.prepare.p : undefined;
  const checksOk = ready?.ok === true;
  const match = matchView(ready, i.device).state === 'match';
  // while a run is on, or after it stopped, the steps before it stay as they were
  const going = running(i.run);
  return [
    ready ? 'done' : i.prepare.kind === 'error' ? 'bad' : 'now',
    !ready ? 'later' : checksOk ? 'done' : 'bad',
    !checksOk ? 'later' : match ? 'done' : i.device.kind === 'connected' ? 'bad' : 'now',
    !match ? 'later' : i.ack || going ? 'done' : 'now',
    !match || !(i.ack || going) ? 'later' : i.run.phase === 'failed' ? 'bad' : 'now',
    i.run.phase === 'failed' ? 'bad' : 'later',
  ];
}

// ---- recovery text (docs/PATCHING.md, "Recovery") ---------------------------------------------------------------------------

export const RECOVERY_STEPS: string[] = [
  'Power off. Hold FUNC and power on. The STARTUP menu comes from the bootloader, which digikit never changes.',
  'Press TRIG 4 (OS UPGRADE). This path takes firmware over MIDI DIN only. USB MIDI is not available there, so connect a DIN MIDI interface to the device’s MIDI IN.',
  'Send the stock .syx of the version that was on the device (choose it above and tick Restore stock, or use any MIDI SysEx sender). Wait until the device restarts.',
  'If the device shows UPGRADE ABORTED, nothing was written: send again.',
];

/** The Prepare step of the RPC route: the device needs no menu. */
export const PREPARE_STEPS_RPC: string[] = [
  'Connect the power adapter.',
  'Keep the USB cable in until the device has restarted. The device does not need to be in any menu.',
];

export const PREPARE_STEPS: string[] = [
  'On the device open SETTINGS > SYSTEM > OS UPGRADE. It shows WAITING FOR SYSEX.',
  'Connect the power adapter.',
  'Keep the USB cable in until the device has restarted.',
];

/** The seconds a send takes (the pace of the sender), for the line above the button. */
export function sendSeconds(p: PreparedFlash): number {
  return Math.round((p.ends.length * p.paceMs) / 1000);
}
