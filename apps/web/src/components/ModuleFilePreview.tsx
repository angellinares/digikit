// Preview of one module file (module.toml `[files] preview`). A registry keyed by the preview name; `oled-1bit` plays a DKBS boot screen on a
// 128x64 canvas. `check` tells before any Send whether the bytes are valid: the error text is the decoder's.
import { Show, createEffect, createSignal, onCleanup, type JSX } from 'solid-js';
import { MAX_FILE_BYTES, TICKS_PER_SECOND, decode } from '@digi/dkbs';
import { DkbsPlayer } from '@digi/dkbs/player';

export interface PreviewProps {
  bytes: Uint8Array;
}

export interface Previewer {
  /** The error text for bytes that this preview cannot show, or undefined when they are valid. */
  check(bytes: Uint8Array): string | undefined;
  Component: (props: PreviewProps) => JSX.Element;
}

const errorText = (e: unknown): string => (e instanceof Error ? e.message : String(e));

function Oled1Bit(props: PreviewProps): JSX.Element {
  let canvas: HTMLCanvasElement | undefined;
  let player: DkbsPlayer | undefined;
  const [playing, setPlaying] = createSignal(false);
  const [loop, setLoop] = createSignal(false);
  const [tick, setTick] = createSignal(0);
  const [total, setTotal] = createSignal(0);
  const [error, setError] = createSignal('');

  createEffect(() => {
    const bytes = props.bytes;
    if (!canvas) return;
    player ??= new DkbsPlayer(canvas);
    player.onTick = (t) => {
      setTick(t);
      setPlaying(player?.isPlaying ?? false);
    };
    try {
      player.load(bytes);
      setTotal(player.ticks);
      setError('');
      player.play();
      setPlaying(player.isPlaying);
    } catch (e) {
      player.pause();
      setPlaying(false);
      setError(errorText(e));
    }
  });
  createEffect(() => {
    if (player) player.loop = loop();
  });
  onCleanup(() => player?.pause());

  const toggle = () => {
    if (!player) return;
    if (player.isPlaying) player.pause();
    else player.play();
    setPlaying(player.isPlaying);
  };

  return (
    <div class="module-preview">
      <canvas
        ref={canvas}
        hidden={!!error()}
        width={128}
        height={64}
        style={{ width: '256px', height: '128px', 'image-rendering': 'pixelated' }}
      />
      <Show when={error()}>
        <p class="err">{error()}</p>
      </Show>
      <Show when={!error()}>
        <div class="acts">
          <button type="button" class="act small" onClick={toggle}>
            {playing() ? 'Pause' : 'Play'}
          </button>
          <label>
            <input type="checkbox" checked={loop()} onChange={(e) => setLoop(e.currentTarget.checked)} /> Loop
          </label>
          <span class="meta">
            tick {tick()} / {total()}
          </span>
          <span class="meta">
            {(total() / TICKS_PER_SECOND).toFixed(2)} s · {props.bytes.length.toLocaleString('en-US')} bytes of{' '}
            {MAX_FILE_BYTES.toLocaleString('en-US')}
          </span>
        </div>
      </Show>
    </div>
  );
}

export const PREVIEWS: Record<string, Previewer> = {
  'oled-1bit': {
    check(bytes) {
      try {
        decode(bytes);
        return undefined;
      } catch (e) {
        return errorText(e);
      }
    },
    Component: Oled1Bit,
  },
};

/** Why `bytes` cannot be sent as a file with this preview; undefined when they are fine or the preview is unknown (nothing to check). */
export const previewError = (preview: string, bytes: Uint8Array): string | undefined => PREVIEWS[preview]?.check(bytes);

export default function ModuleFilePreview(props: { preview: string; bytes: Uint8Array }): JSX.Element {
  return (
    <Show when={PREVIEWS[props.preview]}>
      {(p) => {
        const Preview = p().Component;
        return <Preview bytes={props.bytes} />;
      }}
    </Show>
  );
}
