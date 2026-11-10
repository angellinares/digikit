import { AsyncLocalStorage } from 'node:async_hooks';
import { randomUUID } from 'node:crypto';
import type { Session } from '@digi/test-support';

export interface TimingContext {
  scope?: string;
  detail?: string;
  device?: string;
  session?: string;
}

const spans = new AsyncLocalStorage<TimingContext & { span: string }>();
const sessions = new WeakMap<Session, TimingContext>();

export function setTimingContext(session: Session, context: TimingContext): void {
  sessions.set(session, context);
}

export function timingContext(session: Session): TimingContext {
  return sessions.get(session) ?? { session: session.runDir, device: session.options?.device };
}

/** Inclusive wall time: child records link to their parent and must not be added to it. */
export async function timePhase<T>(phase: string, action: () => Promise<T>, context: TimingContext = {}): Promise<T> {
  const parent = spans.getStore();
  const { span: parentSpan, ...inherited } = parent ?? { span: undefined };
  const span = randomUUID();
  const fields = { ...inherited, ...context };
  const start = performance.now();
  let outcome = 'failure';
  try {
    return await spans.run({ ...fields, span }, async () => {
      const result = await action();
      outcome = 'success';
      return result;
    });
  } finally {
    try {
      console.info(
        JSON.stringify({
          event: 'runtime-phase',
          ...fields,
          phase,
          span,
          parentSpan: parentSpan ?? null,
          durationMs: performance.now() - start,
          durationKind: 'inclusive',
          outcome,
        }),
      );
    } catch {
      // Diagnostics must preserve the operation's original return value or error.
    }
  }
}

/** A phase record for a duration measured elsewhere (inside a package that cannot import this file), with extra fields. */
export function recordPhase(phase: string, durationMs: number, extra: Record<string, unknown> = {}): void {
  const parent = spans.getStore();
  const { span: parentSpan, ...inherited } = parent ?? { span: undefined };
  try {
    console.info(
      JSON.stringify({
        event: 'runtime-phase',
        ...inherited,
        phase,
        span: randomUUID(),
        parentSpan: parentSpan ?? null,
        durationMs,
        durationKind: 'inclusive',
        outcome: 'success',
        ...extra,
      }),
    );
  } catch {
    // Diagnostics must not change the result of the operation.
  }
}
