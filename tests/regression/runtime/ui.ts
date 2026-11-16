import type { LogKindName, RuntimeEvent, RuntimeEvents, Clock, WaitRecord } from '@digi/dk-session';
import { pollUntil, waitClock, type Input, type Key, type Session } from '@digi/test-support';
import { recordPhase } from '../timing.ts';

/** What a `Ui` call waits for after its input. */
export type Until =
  | { event: LogKindName; match?: (e: RuntimeEvent) => boolean; withinMs?: number }
  | { state: () => Promise<unknown>; done: (v: any) => boolean; withinMs?: number; what?: string }
  | { drained: true; withinMs?: number } // status.cf.input_pending === 0 && status.scheduled_inputs === 0
  | { ms: number; why: string }; // explicit fixed wait

const untilKind = (until: Until): string =>
  'event' in until ? 'event' : 'state' in until ? 'state' : 'drained' in until ? 'drained' : 'time';

/** Panel inputs and the waits after them, over one `Input`; the waits use the clock and the events of `rt.listen()`. */
export class Ui {
  private readonly input: Input;
  private readonly clock: Clock;
  private readonly events: () => RuntimeEvents | undefined;
  private readonly record: (r: WaitRecord) => void;
  private readonly session: Pick<Session, 'status'>;

  constructor(
    input: Input,
    clock: Clock,
    events: () => RuntimeEvents | undefined,
    record: (r: WaitRecord) => void,
    session: Pick<Session, 'status'>,
  ) {
    this.input = input;
    this.clock = clock;
    this.events = events;
    this.record = record;
    this.session = session;
  }

  async tap(key: Key, opts: { until: Until; holdMs?: number; held?: Key }): Promise<void> {
    const { until, holdMs = 20, held } = opts;
    const start = performance.now();
    try {
      if (held !== undefined) await this.input.button(held, true);
      try {
        await this.input.tap(key, holdMs);
      } finally {
        if (held !== undefined) await this.input.button(held, false);
      }
      await this.wait(until);
    } finally {
      recordPhase('ui.tap', performance.now() - start, { key, until: untilKind(until) });
    }
  }

  async turn(encoder: Key, detents: number, opts: { until: Until }): Promise<void> {
    const start = performance.now();
    try {
      await this.input.turn(encoder, detents);
      await this.wait(opts.until);
    } finally {
      recordPhase('ui.turn', performance.now() - start, { key: encoder, until: untilKind(opts.until) });
    }
  }

  async wait(until: Until): Promise<void> {
    if ('event' in until) {
      const events = this.events();
      if (!events) throw new Error('call rt.listen() before waiting on an event');
      await events.until(until.event, until.match, { withinMs: until.withinMs ?? 2000 });
    } else if ('state' in until) {
      await pollUntil(this.clock, until.state, until.done, {
        withinMs: until.withinMs ?? 30_000,
        message: until.what ?? 'state',
        intervalMs: 50,
        onWait: this.record,
      });
    } else if ('drained' in until) {
      await pollUntil(
        this.clock,
        async () => {
          const status = await this.session.status();
          return status.cf.input_pending === 0 && status.scheduled_inputs === 0;
        },
        Boolean,
        { withinMs: until.withinMs ?? 30_000, message: 'inputs drained', intervalMs: 50, onWait: this.record },
      );
    } else {
      const start = performance.now();
      const clockStart = await this.clock.now();
      await waitClock(this.clock, until.ms);
      this.record({
        kind: 'time',
        what: until.why,
        clockMs: (await this.clock.now()) - clockStart,
        wallMs: performance.now() - start,
        ok: true,
      });
    }
  }

  /** The same Ui over another input (remote, probe). */
  with(input: Input): Ui {
    return new Ui(input, this.clock, this.events, this.record, this.session);
  }
}
