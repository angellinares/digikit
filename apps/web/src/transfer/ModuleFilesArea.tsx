// Transfer page, Modules mode: the files a module owns (`data/<module id>/`, the module's `[files]` table in module.toml). Shown under the module
// lists for the module whose row was clicked, when it has such a table. On your computer: files chosen or dropped, filtered by the table's `ext`, each checked
// by the preview of the table (a bad file shows its error and cannot be sent). On device: the files of the module's folder, with delete.
// Send is one click: it also sends the module when its .dkm is not on the device. Activating a file stays on the device.
import { For, Show, createEffect, createSignal, on, type JSX } from 'solid-js';
import ModuleFilePreview, { previewError } from '../components/ModuleFilePreview';
import { errorLine, formatBytes } from '../modules/backend';
import { ListRow, LocalPane, DevicePane, createConfirm, DeleteButton, writing, type PageCtx } from './TransferParts';
import type { ModuleFilesSpec } from './env';
import { createPaneSelect } from './pane-select';
import SplitPane from './SplitPane';
import { transferBackend } from './transfer-backend';
import { countLabel } from './transfer-core';
import { lastView } from './transfer-state';

const SENT_NOTE = "Choose it in the device's module settings; it applies on next restart.";

interface Local {
  name: string;
  bytes: Uint8Array;
  error: string | undefined;
}

export default function ModuleFilesArea(props: { ctx: PageCtx; id: string; spec: ModuleFilesSpec }): JSX.Element {
  const { busy, run, setError } = props.ctx;
  let input: HTMLInputElement | undefined;
  const [local, setLocal] = createSignal<Local[]>([]);
  const [device, setDevice] = createSignal<{ name: string; size: number }[]>([]);
  const [lsel, setLsel] = createSignal<Set<string>>(new Set());
  const [dsel, setDsel] = createSignal<Set<string>>(new Set());
  const [shown, setShown] = createSignal<{ name: string; bytes: Uint8Array }>();
  const [note, setNote] = createSignal('');
  const confirm = createConfirm();
  // a list or read that finishes after the module or the view changed is dropped
  let listSeq = 0;
  let readSeq = 0;

  const accept = props.spec.ext.map((e) => `.${e}`).join(',');
  const wanted = (name: string) => props.spec.ext.some((e) => name.toLowerCase().endsWith(`.${e.toLowerCase()}`));

  // the device's list is read again whenever the page read (the view changes) and for another module
  createEffect(
    on(
      () => [props.id, lastView()] as const,
      async ([id, view]) => {
        const mine = ++listSeq;
        if (!view) {
          setDevice([]);
          return;
        }
        try {
          const files = await transferBackend.listModuleFiles(id);
          if (mine === listSeq) setDevice(files);
        } catch (e) {
          if (mine !== listSeq) return;
          setDevice([]);
          setError(errorLine(e));
        }
      },
    ),
  );
  // another module: what was chosen for the old one is not kept
  createEffect(
    on(
      () => props.id,
      () => {
        readSeq += 1;
        setLocal([]);
        setLsel(new Set<string>());
        setDsel(new Set<string>());
        setShown(undefined);
        setNote('');
      },
      { defer: true },
    ),
  );

  const addFiles = async (files: File[]) => {
    const picked = files.filter((f) => wanted(f.name));
    if (!picked.length) {
      setError(`Only ${props.spec.ext.map((e) => `.${e}`).join(', ')} files can be added.`);
      return;
    }
    const added: Local[] = await Promise.all(
      picked.map(async (f) => {
        const bytes = new Uint8Array(await f.arrayBuffer());
        return { name: f.name, bytes, error: previewError(props.spec.preview, bytes) };
      }),
    );
    setLocal((cur) => [...cur.filter((c) => !added.some((a) => a.name === c.name)), ...added]);
    setShown({ name: added[0]!.name, bytes: added[0]!.bytes });
    setNote('');
  };

  const ticked = () => local().filter((l) => lsel().has(l.name));
  const canSend = () => ticked().length > 0 && ticked().every((l) => !l.error);
  const send = () =>
    run('Sending', async () => {
      const files = ticked();
      const row = lastView()?.modules.find((r) => r.id === props.id);
      // the module goes first when its .dkm is not on the device
      if (row && row.on_card === 'no') await transferBackend.install([props.id], () => {});
      for (const f of files) {
        setNote(`Sending ${f.name}…`);
        await transferBackend.putModuleFile(props.id, f.name, f.bytes, (sent, total) =>
          setNote(`Sending ${f.name}… ${Math.round((100 * sent) / total)}%`),
        );
      }
      setNote(SENT_NOTE);
      setLsel(new Set<string>());
    });
  const removeSelected = () =>
    run('Deleting', async () => {
      for (const name of dsel()) await transferBackend.removeModuleFile(props.id, name);
      setDsel(new Set<string>());
      setShown(undefined);
    });
  const showDevice = (name: string) => {
    const mine = ++readSeq;
    void transferBackend.readModuleFile(props.id, name).then(
      (bytes) => {
        if (mine === readSeq) setShown({ name, bytes });
      },
      (e) => {
        if (mine === readSeq) setError(errorLine(e));
      },
    );
  };

  const lpane = createPaneSelect({
    keys: () => local().map((l) => l.name),
    selected: lsel,
    set: setLsel,
    disabled: () => writing(busy()),
  });
  const dpane = createPaneSelect({
    keys: () => device().map((d) => d.name),
    selected: dsel,
    set: (next) => {
      setDsel(next);
      confirm.stop();
    },
    disabled: () => writing(busy()),
  });

  return (
    <div class="group">
      <h4>{props.spec.label}</h4>
      <SplitPane id="transfer-files">
        <LocalPane
          onDrop={(f) => void addFiles(f)}
          select={lpane}
          selectAll={<span class="tick" />}
          actions={
            <>
              <input
                ref={input}
                type="file"
                accept={accept}
                multiple
                hidden
                onChange={(e) => {
                  const files = Array.from(e.currentTarget.files ?? []);
                  e.currentTarget.value = '';
                  void addFiles(files);
                }}
              />
              <button type="button" class="act small" disabled={!!busy()} onClick={() => input?.click()}>
                Choose…
              </button>
              <span class="drop">or drop files</span>
              <button
                type="button"
                class="act primary"
                disabled={!!busy() || !canSend()}
                title={
                  canSend()
                    ? 'Send the ticked files to the device (and the module, when it is not there)'
                    : 'Tick a valid file to send'
                }
                onClick={() => void send()}
              >
                {busy() === 'Sending' ? 'Sending…' : countLabel('Send', ticked().length)}
              </button>
            </>
          }
        >
          <Show when={local().length} fallback={<p class="empty">No files chosen.</p>}>
            <ul class="rows">
              <For each={local()}>
                {(l) => (
                  <ListRow
                    rowKey={l.name}
                    selected={lsel().has(l.name)}
                    onSelect={(e) => {
                      lpane.click(l.name, e);
                      setShown({ name: l.name, bytes: l.bytes });
                    }}
                    check={{
                      label: `Send ${l.name}`,
                      checked: lsel().has(l.name),
                      disabled: writing(busy()),
                      onChange: () => lpane.tick(l.name),
                    }}
                    name={l.name}
                    tags={l.error ? [{ text: 'invalid', tone: 'warn', title: l.error }] : []}
                    size={formatBytes(l.bytes.length)}
                  />
                )}
              </For>
            </ul>
          </Show>
        </LocalPane>
        <DevicePane
          ctx={props.ctx}
          hideTarget
          select={dpane}
          selectAll={<span class="tick" />}
          actions={
            <DeleteButton
              n={dsel().size}
              busy={busy()}
              confirming={confirm.confirming()}
              title="Delete the ticked files from the device"
              onClick={() => confirm.click(() => void removeSelected())}
            />
          }
        >
          <Show when={device().length} fallback={<p class="empty">The device holds no files of this module.</p>}>
            <ul class="rows">
              <For each={device()}>
                {(d) => (
                  <ListRow
                    rowKey={d.name}
                    selected={dsel().has(d.name)}
                    onSelect={(e) => {
                      dpane.click(d.name, e);
                      showDevice(d.name);
                    }}
                    check={{
                      label: `Delete ${d.name}`,
                      checked: dsel().has(d.name),
                      disabled: writing(busy()),
                      onChange: () => dpane.tick(d.name),
                    }}
                    name={d.name}
                    title={`data/${props.id}/${d.name}`}
                    size={formatBytes(d.size)}
                  />
                )}
              </For>
            </ul>
          </Show>
        </DevicePane>
      </SplitPane>
      <Show when={note()}>
        <p class="meta">{note()}</p>
      </Show>
      <Show when={shown()}>
        {(f) => (
          <>
            <p class="meta">{f().name}</p>
            <ModuleFilePreview preview={props.spec.preview} bytes={f().bytes} />
          </>
        )}
      </Show>
    </div>
  );
}
