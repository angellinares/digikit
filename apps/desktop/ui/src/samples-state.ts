// The Samples mode's state, kept for the life of the window (the rest of the Transfer page's state is apps/web/src/transfer/transfer-state.ts).
import { createSignal } from 'solid-js';
import type { Prepared } from '@digi/dk-session/dksample';
import type { SampleEntry } from '@digi/dk-session/samples';
import { remembered } from '../../../web/src/transfer/transfer-state';
import type { LocalEntry, Uploads } from './samples-core';

/** The folder on the computer that Samples mode lists ('' until one is chosen). */
export const [localFolder, setLocalFolder] = remembered('digikit.samplesFolder', (v) => v ?? '');

/** Samples mode, On your computer: the open level of the chosen folder, its entries, what each file will become, the files added by hand,
 * and the ticks. */
export const [localPath, setLocalPath] = createSignal('');
export const [localEntries, setLocalEntries] = createSignal<LocalEntry[]>();
export const [localKinds, setLocalKinds] = createSignal<Record<string, Prepared['kind']>>({});
export const [addedFiles, setAddedFiles] = createSignal<File[]>([]);
export const [localTicks, setLocalTicks] = createSignal<Set<string>>(new Set());

/** Samples mode, On device: the open folder ('' is the top), its listing, the ticked paths, and the uploads of this window. */
export const [sampleFolder, setSampleFolder] = createSignal('');
export const [sampleEntries, setSampleEntries] = createSignal<SampleEntry[]>();
export const [sampleTicks, setSampleTicks] = createSignal<Set<string>>(new Set());
export const [uploads, setUploads] = createSignal<Uploads>({});
/** The uploads as they were when the running (or last) samples upload started: the bar counts what changed since. */
export const [uploadBase, setUploadBase] = createSignal<Uploads>({});
/** The speed of the running upload (bytes per second) and the average of the last run; undefined: none to show. */
export const [uploadSpeed, setUploadSpeed] = createSignal<{ rate?: number; average?: number }>({});
