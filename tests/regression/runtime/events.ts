import { LOG_KIND, LOG_KINDS } from '@digi/dk-session/gen_proto';
import { Events, type LogEvent } from '@digi/test-support';
import type { PatchedSession } from '../support.ts';

/**
 * Listen to the runtime log of a booted session: the cursor starts at its newest event and `digikit/remote` and `digikit/debug` log at debug
 * level (3). Needs `digikit/debug` loaded (installWithDebug). Wait with `events.until(kind, predicate, { emulatedMs })`; `LOG_KIND` has the kinds.
 */
export async function listenEvents({ session, dk }: PatchedSession): Promise<Events> {
  const events = new Events(dk, session, {
    bootStart: LOG_KIND.BOOT_START,
    name: (kind) => LOG_KINDS[kind]?.[0] ?? `KIND${kind}`,
  });
  await events.listen();
  return events;
}

/** One remote detent count on `encoder` (digikit/remote loaded): the turn, then the firmware decoder's INPUT_TAKEN for it. Returns the delta taken (0: the module took none). */
export async function remoteTurnTaken(
  { dk }: Pick<PatchedSession, 'dk'>,
  events: Events,
  encoder: number,
  delta: number,
  { emulatedMs = 2000 } = {},
): Promise<number> {
  const took = await dk.remoteTurnOnce(encoder, delta);
  if (took !== 0) {
    await events.until(
      LOG_KIND.INPUT_TAKEN,
      (e: LogEvent) => e.args[0] === 2 && e.args[1] === encoder && (e.args[2]! | 0) === took,
      { emulatedMs },
    );
  }
  return took;
}
