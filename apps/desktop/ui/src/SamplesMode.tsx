// Transfer page, Samples mode, a file explorer on both sides. On your computer: a folder of samples (chosen once, remembered; one level shown
// with a breadcrumb) and the files added by hand or dropped (kept in the list even when they are outside the folder). On device: the device's
// sample folders. Rows are selected like in Finder (click, Shift, Cmd or Ctrl, a marquee, Cmd+A, Esc; the tick is the selection), a folder
// opens with a double click or Enter, "Up" and the breadcrumb go back. Upload sends the selected rows, a folder with everything in it and its
// structure kept, into the open device folder; so does a drag of the rows onto the device pane or onto one of its folders. The backend is the
// one the device profile names (`[storage] samples`: the stock MidiRpc file commands or the digikit store; samples-backend.ts), so the mode is
// the same on every device. A WAV is converted to the native sample file before it goes; a native sample goes as it is; any other file is
// uploaded unchanged. Each upload is verified against the bytes sent. The page speaks the protocol in TypeScript over the selected target; the
// local files come through the native `local_*` commands (local.rs); no process is started.
import { For, Show, createSignal, type JSX } from 'solid-js';
import { errorLine, formatBytes, isCancelled } from '@web/modules/backend';
import { endRun, openDeviceSamples, labelOf, listLocalAt, pickFolder, readLocalFile } from './samples-backend';
import {
  applyUpload,
  confirmDeleteLabel,
  crumbs,
  deletePartLine,
  deleteRows,
  deletingLine,
  dropUploads,
  folderNameError,
  joinPath,
  foldersFromOutside,
  localCrumbs,
  localRowTags,
  localRows,
  noFoldersMessage,
  parentOf,
  planUpload,
  sampleRows,
  samplesBar,
  selectedRows,
  uploadCancelled,
  uploadFailure,
  uploadItems,
  type LocalRow,
  type UploadItem,
} from './samples-core';
import {
  Crumbs,
  DevicePane,
  DeleteButton,
  ListRow,
  LocalPane,
  SelectAll,
  UpRow,
  createConfirm,
  writing,
  droppedFiles,
  type CancelledRun,
  type PageCtx,
} from '@web/transfer/TransferParts';
import { createPaneSelect } from '@web/transfer/pane-select';
import { ROWS_TYPE, dropFolder } from '@web/transfer/select-core';
import { noteActivity } from '@web/transfer/activity';
import { countLabel } from '@web/transfer/transfer-core';
import SplitPane from '@web/transfer/SplitPane';
import { askStop, deviceInfo, stopWasAsked } from '@web/transfer/transfer-state';
import {
  addedFiles,
  localEntries,
  localFolder,
  localKinds,
  localPath,
  localTicks,
  sampleEntries,
  sampleFolder,
  sampleTicks,
  setAddedFiles,
  setLocalFolder,
  setLocalKinds,
  setLocalPath,
  setLocalTicks,
  setSampleFolder,
  setSampleTicks,
  setUploadAverage,
  setUploadBase,
  setUploadNotes,
  setUploads,
  uploads,
} from './samples-state';

export default function SamplesMode(props: { ctx: PageCtx }): JSX.Element {
  const { busy, run, setError } = props.ctx;
  const [note, setNote] = createSignal('');
  const [naming, setNaming] = createSignal(false);
  const [newName, setNewName] = createSignal('');
  let input: HTMLInputElement | undefined;
  const confirm = createConfirm();
  /** The progress line of a folder delete (empty when none runs). */
  const [deleting, setDeleting] = createSignal('');

  // ---- On your computer ------------------------------------------------------------------------------------------------------------
  const lrows = () =>
    localRows(
      localPath(),
      localEntries() ?? [],
      addedFiles().map((f) => ({ name: f.name, size: f.size })),
      localKinds(),
    );
  const picked = () => selectedRows(lrows(), localTicks());
  const lsel = createPaneSelect({
    keys: () => lrows().map((r) => r.key),
    selected: localTicks,
    set: setLocalTicks,
    disabled: () => writing(busy()),
    open: (key) => {
      const r = lrows().find((x) => x.key === key);
      if (r?.kind === 'folder') openLocal(r.path);
    },
    up: () => localPath() && openLocal(parentOf(localPath())),
  });

  const choose = async () => {
    try {
      const dir = await pickFolder(localFolder());
      if (!dir) return;
      setLocalFolder(dir);
      setLocalPath('');
      setLocalTicks(new Set<string>());
      void run('Reading', async () => {});
    } catch (e) {
      setError(errorLine(e));
    }
  };
  const openLocal = (path: string) => {
    if (writing(busy())) return;
    setLocalPath(path);
    setLocalTicks(new Set<string>());
    void run('Reading', async () => {});
  };
  /** Files added by hand join the list (same name: the newer one) and are ticked; their first bytes tell what the upload will do. */
  const addFiles = (files: File[], folders = 0) => {
    if (folders) setError(foldersFromOutside(folders));
    if (!files.length) return;
    const names = new Set(files.map((f) => f.name));
    setAddedFiles((old) => [...old.filter((f) => !names.has(f.name)), ...files]);
    setLocalTicks((t) => new Set([...t, ...files.map((f) => `+${f.name}`)]));
    for (const f of files)
      void labelOf(async () => new Uint8Array(await f.slice(0, 64).arrayBuffer()), f.size).then((k) => {
        if (k) setLocalKinds((all) => ({ ...all, [`+${f.name}`]: k }));
      });
  };
  /** Send `items` (and make `mkdirs` first) into the device; the bar counts them all. */
  const send = async (mkdirs: string[], items: UploadItem[]): Promise<boolean> => {
    let done = false;
    try {
      await run('Uploading', async (): Promise<void | CancelledRun> => {
        const o = await openDeviceSamples(true);
        if (mkdirs.length && !(await o.backend.canMkdir())) throw new Error(noFoldersMessage(o.name));
        setNote('');
        const base = uploads();
        setUploadBase(base);
        setUploadNotes([]);
        const started = Date.now();
        setUploadAverage(undefined);
        await uploadItems(
          o.backend,
          o.prepare,
          mkdirs,
          items,
          (e) => {
            noteActivity();
            if (e.event === 'note') setUploadNotes((n) => [...n, e.message]);
            setUploads((u) => applyUpload(u, e));
          },
          stopWasAsked,
        );
        // the run's bytes over its time, for the end text of the bar
        const sent = samplesBar(uploads(), base, false)?.sent ?? 0;
        setUploadAverage(sent > 0 && Date.now() > started ? sent / ((Date.now() - started) / 1000) : undefined);
        // the real error of a file is on the row only; the run shows and logs it, and the selection stays for a retry
        const failure = uploadFailure(uploads(), base);
        if (failure) throw new Error(failure);
        // stopped by the user: not an error; the bar and the Log say what was done, and the selection stays for a retry
        const stopped = uploadCancelled(uploads(), base);
        if (stopped) return { cancelled: stopped };
        done = true;
      });
    } finally {
      endRun();
    }
    return done;
  };
  const itemOf = (folder: string, name: string, size: number, read: () => Promise<Uint8Array>): UploadItem => ({
    folder,
    name,
    size,
    read,
  });
  /** A read (a window that got the focus, the light poll) ends within moments; a drop that lands in it waits for the end. */
  const whenIdle = async () => {
    for (let i = 0; i < 200 && busy() === 'Reading'; i++) await new Promise((r) => setTimeout(r, 50));
  };
  /** Upload local rows (files, and folders with everything in them) into the device folder `target`. */
  const uploadRows = async (rows: LocalRow[], target: string) => {
    await whenIdle();
    if (!rows.length || busy()) return;
    let plan;
    try {
      plan = await planUpload(rows, target, listLocalAt);
    } catch (e) {
      setError(errorLine(e));
      return;
    }
    const items = plan.files.map((f) =>
      itemOf(f.folder, f.name, f.size, async () =>
        f.added
          ? new Uint8Array(await (addedFiles().find((a) => a.name === f.name) as File).arrayBuffer())
          : await readLocalFile(f.rel),
      ),
    );
    // the selection stays when the run could not start (no folders on the device, a link that is gone)
    if (await send(plan.mkdirs, items)) setLocalTicks(new Set<string>());
  };
  const upload = () => void uploadRows(picked(), sampleFolder());
  /** Files dragged in from outside onto the device pane go straight into the folder they were dropped on. */
  const uploadDropped = async (files: File[], target: string) => {
    await whenIdle();
    if (!files.length || busy()) return;
    await send(
      [],
      files.map((f) => itemOf(target, f.name, f.size, async () => new Uint8Array(await f.arrayBuffer()))),
    );
  };
  const onDeviceDrop = (kind: 'rows' | 'files', folder: string | undefined, e: DragEvent) => {
    const target = dropFolder(folder, sampleFolder());
    if (kind === 'files') {
      const d = droppedFiles(e);
      if (d.folders) setError(foldersFromOutside(d.folders));
      void uploadDropped(d.files, target);
      return;
    }
    let keys: string[] = [];
    try {
      keys = JSON.parse(e.dataTransfer?.getData(ROWS_TYPE) ?? '[]') as string[];
    } catch {
      // not our drag
    }
    const wanted = new Set(keys);
    void uploadRows(
      lrows().filter((r) => wanted.has(r.key)),
      target,
    );
  };

  // ---- On device -------------------------------------------------------------------------------------------------------------------
  const open = (folder: string) => {
    if (writing(busy())) return;
    setSampleFolder(folder);
    setSampleTicks(new Set<string>());
    setNote('');
    confirm.stop();
    void run('Reading', async () => {});
  };
  const createFolder = () => {
    const name = newName().trim();
    const why = folderNameError(name, sampleEntries() ?? []);
    if (why) {
      setError(why);
      return;
    }
    setNaming(false);
    setNewName('');
    void run('Creating', async () => {
      await (await openDeviceSamples()).backend.mkdir(joinPath(sampleFolder(), name));
    });
  };
  const rows = () => sampleRows(sampleFolder(), sampleEntries() ?? [], uploads());
  const nDelete = () => deleteRows(rows(), sampleTicks()).length;
  const removeSelected = () =>
    run('Deleting', async (): Promise<void | CancelledRun> => {
      const { backend } = await openDeviceSamples();
      const gone = deleteRows(rows(), sampleTicks());
      setNote('');
      let done = 0;
      let total = 0;
      const forget = (path: string) => {
        setUploads((u) => dropUploads(u, [path]));
        setSampleTicks((t) => {
          const next = new Set(t);
          next.delete(path);
          return next;
        });
      };
      try {
        for (const r of gone) {
          if (r.kind === 'folder') {
            // the folder's files one by one, then the folder: Cancel stops between two files
            const res = await backend.removeTree(r.path, {
              onProgress: (d, t) => {
                done = d;
                total = t;
                setDeleting(deletingLine(d, t, gone.length > 1 ? r.name : undefined));
              },
              shouldStop: stopWasAsked,
            });
            if (res.stopped) return { cancelled: `${r.name}: ${deletePartLine(res.removed, res.total)}` };
          } else {
            if (stopWasAsked()) return { cancelled: `${r.name} and later rows not deleted` };
            await backend.remove(sampleFolder(), r.name);
          }
          forget(r.path);
          done = total = 0;
        }
      } catch (e) {
        // the Cancel of the wait notice ends the request in flight; a failure names the file and says what is left of the folder
        if (isCancelled(e)) return { cancelled: total ? deletePartLine(done, total) : 'stopped' };
        if (total) throw new Error(`${errorLine(e)} (${deletePartLine(done, total)})`, { cause: e });
        throw e;
      } finally {
        setDeleting('');
      }
      setNote(`Deleted ${gone.length} item${gone.length === 1 ? '' : 's'}.`);
    });
  const dsel = createPaneSelect({
    keys: () =>
      rows()
        .filter((r) => !r.pending)
        .map((r) => r.path),
    selected: sampleTicks,
    set: (next) => {
      setSampleTicks(next);
      confirm.stop();
    },
    disabled: () => writing(busy()),
    open: (key) => {
      const r = rows().find((x) => x.path === key);
      if (r?.kind === 'folder' && !r.pending) open(r.path);
    },
    up: () => sampleFolder() && open(parentOf(sampleFolder())),
  });

  return (
    <SplitPane id="transfer">
      <LocalPane
        onDrop={addFiles}
        select={lsel}
        selectAll={
          <SelectAll
            label="Select all to upload"
            keys={lrows().map((r) => r.key)}
            ticked={localTicks()}
            disabled={writing(busy())}
            onChange={setLocalTicks}
          />
        }
        actions={
          <>
            <input
              ref={input}
              type="file"
              multiple
              hidden
              onChange={(e) => {
                const files = Array.from(e.currentTarget.files ?? []);
                e.currentTarget.value = '';
                addFiles(files);
              }}
            />
            <button
              type="button"
              class="act small"
              disabled={!!busy()}
              title="Choose the folder on this computer that holds your samples"
              onClick={() => void choose()}
            >
              Choose folder…
            </button>
            <button
              type="button"
              class="act small"
              disabled={!!busy()}
              title="Add files to the list, from any folder"
              onClick={() => input?.click()}
            >
              Add files…
            </button>
            <span class="drop">or drop files</span>
            <button
              type="button"
              class="act primary"
              disabled={!!busy() || picked().length === 0 || !deviceInfo()}
              title={
                picked().length
                  ? 'Convert, upload and verify the selected files and folders into the open folder of the device'
                  : 'Select a file or a folder to upload'
              }
              onClick={() => void upload()}
            >
              {busy() === 'Uploading' ? 'Uploading…' : countLabel('Upload', picked().length)}
            </button>
          </>
        }
      >
        <Show when={localFolder()}>
          <Crumbs
            items={localCrumbs(localFolder(), localPath())}
            current={localPath()}
            disabled={writing(busy())}
            label="Folder on your computer"
            onOpen={openLocal}
          />
        </Show>
        <Show when={localFolder() && localPath()}>
          <UpRow
            disabled={writing(busy())}
            title="Go up one folder (Backspace)"
            onUp={() => openLocal(parentOf(localPath()))}
          />
        </Show>
        <Show
          when={lrows().length}
          fallback={
            <p class="empty">
              {localFolder()
                ? 'Nothing here.'
                : 'Choose a folder, or add files. A WAV is converted when it is uploaded.'}
            </p>
          }
        >
          <ul class="rows">
            <For each={lrows()}>
              {(r) => (
                <ListRow
                  rowKey={r.key}
                  selected={localTicks().has(r.key)}
                  onSelect={(e) => lsel.click(r.key, e)}
                  check={{
                    label: `Upload ${r.name}`,
                    checked: localTicks().has(r.key),
                    disabled: writing(busy()),
                    onChange: () => lsel.tick(r.key),
                  }}
                  name={r.name}
                  title={r.added ? `${r.name} (added)` : r.path}
                  folder={
                    r.kind === 'folder' ? { disabled: writing(busy()), onOpen: () => openLocal(r.path) } : undefined
                  }
                  drag={writing(busy()) ? undefined : (e) => lsel.dragStart(r.key, e)}
                  tags={localRowTags(r)}
                  size={r.kind === 'file' ? formatBytes(r.size) : ''}
                />
              )}
            </For>
          </ul>
        </Show>
      </LocalPane>

      <DevicePane
        ctx={props.ctx}
        select={dsel}
        drop={{ onDrop: onDeviceDrop, disabled: () => writing(busy()) || !deviceInfo() }}
        selectAll={
          <SelectAll
            label="Select all to delete"
            keys={rows()
              .filter((r) => !r.pending)
              .map((r) => r.path)}
            ticked={sampleTicks()}
            disabled={writing(busy())}
            onChange={(next) => {
              setSampleTicks(next);
              confirm.stop();
            }}
          />
        }
        actions={
          <>
            <Show when={deviceInfo()?.canMkdir}>
              <button type="button" class="act" disabled={!!busy()} onClick={() => setNaming(!naming())}>
                New folder
              </button>
            </Show>
            <DeleteButton
              n={nDelete()}
              busy={busy()}
              confirming={confirm.confirming()}
              confirmLabel={confirmDeleteLabel(deleteRows(rows(), sampleTicks()))}
              title="Delete the selected files and folders, with everything in them, from the device"
              onClick={() => confirm.click(() => void removeSelected())}
            />
          </>
        }
      >
        <Crumbs
          items={crumbs(sampleFolder())}
          current={sampleFolder()}
          disabled={writing(busy())}
          label="Folder on the device"
          onOpen={open}
        />
        <Show when={deleting()}>
          <p class="empty note" role="status">
            <span>{deleting()}</span>{' '}
            <button type="button" class="act small" onClick={askStop}>
              Cancel
            </button>
          </p>
        </Show>
        <Show when={sampleFolder()}>
          <UpRow
            disabled={writing(busy())}
            title="Go up one folder (Backspace); a drop here goes into that folder"
            dropPath={parentOf(sampleFolder())}
            onUp={() => open(parentOf(sampleFolder()))}
          />
        </Show>
        <Show when={naming()}>
          <form
            class="newfolder"
            onSubmit={(e) => {
              e.preventDefault();
              createFolder();
            }}
          >
            <input
              type="text"
              aria-label="New folder name"
              placeholder="folder name"
              value={newName()}
              onInput={(e) => setNewName(e.currentTarget.value)}
            />
            <button type="submit" class="act small" disabled={!!busy()}>
              Create
            </button>
          </form>
        </Show>
        <Show when={sampleEntries()} fallback={<p class="empty">{busy() ? `${busy()}…` : 'No listing yet.'}</p>}>
          <Show when={rows().length} fallback={<p class="empty">This folder is empty.</p>}>
            <ul class="rows">
              <For each={rows()}>
                {(r) => (
                  <ListRow
                    rowKey={r.pending ? undefined : r.path}
                    selected={sampleTicks().has(r.path)}
                    onSelect={r.pending ? undefined : (e) => dsel.click(r.path, e)}
                    check={{
                      label: `Delete ${r.name}`,
                      checked: sampleTicks().has(r.path),
                      disabled: writing(busy()) || r.pending,
                      onChange: () => dsel.tick(r.path),
                    }}
                    name={r.name}
                    title={r.path}
                    folder={
                      r.kind === 'folder'
                        ? { disabled: writing(busy()) || r.pending, onOpen: () => open(r.path) }
                        : undefined
                    }
                    dropPath={r.kind === 'folder' && !r.pending ? r.path : undefined}
                    size={r.kind === 'file' ? formatBytes(r.size) : ''}
                    status={r.status}
                  />
                )}
              </For>
            </ul>
          </Show>
        </Show>
        <Show when={note()}>
          <p class="empty note">{note()}</p>
        </Show>
      </DevicePane>
    </SplitPane>
  );
}
