// Faceplate-only client for live.vite.config.ts and its running control socket.
import { Show, createSignal, onCleanup, onMount } from 'solid-js';
import { render } from 'solid-js/web';
import '@fontsource-variable/archivo/wdth.css';
import '@fontsource/jetbrains-mono/400.css';
import '@web/styles/site.css';
import LivePanel, { type LivePanelApi } from '@web/components/LivePanel';
import { panels } from '@web/data/faceplates';
import { parseLeds } from '@web/runtime/leds';
import type { RuntimeStatus } from '@web/runtime/runtime';
import './panel.css';

async function call<T>(method: string, params: Record<string, unknown> = {}): Promise<T> {
  const response = await fetch('/__emu', {
    method: 'POST',
    body: JSON.stringify({ method, params }),
    signal: AbortSignal.timeout(12_000),
  });
  if (!response.ok) throw new Error(await response.text());
  return response.json() as Promise<T>;
}

function Faceplate() {
  const [device, setDevice] = createSignal<'dt2' | 'dn2'>();
  const [error, setError] = createSignal('Connecting to the emulator…');
  let panel: LivePanelApi | undefined;
  let connected = false;
  let alive = true;
  let timer: number | undefined;
  let frameRevision: number | undefined;
  let ledRevision: number | undefined;
  let inputs = Promise.resolve();
  // A lost reply can still have applied a press. Forget a key only after its release is acknowledged.
  const held = new Set<number>();
  // Preserve press/release and chord order, including rapid encoder pushes.
  const input = (method: string, params: Record<string, unknown>) => {
    inputs = inputs
      .then(async () => {
        if (!connected) throw new Error('Emulator disconnected; reconnecting…');
        if (method === 'input.button' && params.down) held.add(params.code as number);
        await call(method, params);
        if (method === 'input.button' && !params.down) held.delete(params.code as number);
      })
      .catch((cause) => {
        connected = false;
        panel?.releaseAll();
        setError(String(cause));
      });
    return inputs;
  };
  const runtime = {
    press: (code: number) => input('input.button', { code, down: true }),
    release: (code: number) => input('input.button', { code, down: false }),
    turn: (encoder: number, detents: number) => input('input.turn', { encoder, detents }),
  };
  const poll = async () => {
    try {
      const status = await call<{ cf?: RuntimeStatus; frame_revision?: number | null }>('status');
      if (!alive) return;
      const cf = status.cf;
      if (!cf || (cf.device !== 'dt2' && cf.device !== 'dn2')) throw new Error('Waiting for emulator firmware…');
      if (device() !== cf.device) {
        panel?.releaseAll();
        setDevice(cf.device);
        frameRevision = ledRevision = undefined;
      }
      if (!connected) {
        // Drain the old queue first: an in-flight press may have reached the same daemon.
        await inputs;
        for (const code of held) {
          await call('input.button', { code, down: false });
          held.delete(code);
        }
        if (!alive) return;
        panel?.releaseAll();
        connected = true;
      }
      if (status.frame_revision != null && status.frame_revision !== frameRevision) {
        const frame = await call<{ revision: number; data_b64: string }>('framebuffer.get');
        if (!alive) return;
        const bytes = Uint8Array.from(atob(frame.data_b64), (c) => c.charCodeAt(0));
        if (bytes.length !== 1024) throw new Error(`Invalid OLED frame: ${bytes.length} bytes`);
        panel?.showFrame(bytes);
        frameRevision = frame.revision;
      }
      if (cf.led_revision !== ledRevision || ledRevision === undefined) {
        const leds = await call<{ led_revision: number; leds: string }>('leds.get');
        if (!alive) return;
        panel?.previewLeds(parseLeds(leds.leds));
        ledRevision = leds.led_revision;
      }
      setError('');
    } catch (cause) {
      if (!alive) return;
      connected = false;
      panel?.releaseAll();
      panel?.clear();
      frameRevision = ledRevision = undefined;
      setError(String(cause));
    } finally {
      // One poll in flight, at most 20 Hz; disconnected daemons retry at 1 Hz.
      if (alive) timer = window.setTimeout(() => void poll(), connected ? 50 : 1000);
    }
  };
  onMount(() => void poll());
  onCleanup(() => {
    alive = false;
    window.clearTimeout(timer);
    panel?.releaseAll();
  });
  return (
    <main class="faceplate-stage" aria-label="Emulator front panel">
      <Show when={device()} keyed>
        {(name) => (
          <div class="faceplate" style={{ '--accent': panels[name].accent, '--func-legend': panels[name].func_legend }}>
            <LivePanel
              panel={panels[name]}
              runtime={() => (connected ? runtime : undefined)}
              onError={setError}
              ref={(api) => (panel = api)}
            />
          </div>
        )}
      </Show>
      <Show when={error()}>
        <p class="connection-error" role="status">
          {error()}
        </p>
      </Show>
    </main>
  );
}

render(() => <Faceplate />, document.getElementById('app')!);
