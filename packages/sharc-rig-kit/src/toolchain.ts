/**
 * Assembling and linking SHARC+ sources from TypeScript: the same `patchkit sharc` commands as tools/patchkit/pk_sharc.py.
 * `patchkit sharc inc` expands the `#include` lines, `asm` assembles with the build's instruction checks, `link` places an object at
 * a short-word address against externs and verifies it. A repeated object (the adapter, shared by many modules) is assembled once
 * per process.
 */
import { execFileSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { existsSync, readFileSync } from 'node:fs';
import path from 'node:path';
import { parse } from 'smol-toml';
import { ROOT } from './rig.ts';

/** The loader byte address of short-word 0 (tools/sharc_dev/sharcldr.py). */
export const SW_ALIAS_BASE = 0x28000000;

export class ToolchainError extends Error {}

/** The patchkit binary: `DIGI_PATCHKIT`, else dist/patchkit/patchkit (`nx run patchkit:cli`). */
export function patchkitBinary(): string {
  const binary = process.env.DIGI_PATCHKIT || path.join(ROOT, 'dist/patchkit/patchkit');
  if (!existsSync(binary)) throw new Error(`missing ${binary}: run nx run patchkit:cli`);
  return binary;
}

/** `patchkit ARGS` (stdout); a failure is a ToolchainError with the binary's message. */
export function patchkit(args: string[], stdin?: string): string {
  try {
    return execFileSync(patchkitBinary(), ['--root', ROOT, ...args], {
      cwd: ROOT,
      input: stdin,
      encoding: 'utf8',
      maxBuffer: 256 << 20,
      stdio: ['pipe', 'pipe', 'pipe'],
    });
  } catch (error) {
    const e = error as { stderr?: string; stdout?: string; message: string };
    throw new ToolchainError((e.stderr || e.stdout || e.message).trim().replace(/^patchkit: /, ''));
  }
}

export interface SharcObject {
  /** the assembled source text (a link starts from it again) */
  text: string;
  /** code length in parcels (16 bits) */
  parcels: number;
  /** defined symbol -> parcel offset */
  symbols: Record<string, number>;
  /** data section -> [bytes in memory order, is bss] */
  data: Record<string, { bytes: Uint8Array; bss: boolean; align?: number }>;
  /** data symbol -> [section, byte offset] */
  dataSymbols: Record<string, { section: string; offset: number }>;
}

export interface Linked {
  baseSw: number;
  /** boot-stream hex (little endian per parcel) */
  codeBoot: string;
  /** symbol -> short-word address */
  symbols: Record<string, number>;
  /** instructions the link re-decoded */
  checked?: number;
}

/** The include directories of an object of a module: its generated and own sources, then the shared ones of pins.toml. */
export function sharcIncludeDirs(moddir: string): string[] {
  const pins = parse(readFileSync(path.join(ROOT, 'tools/patchkit/toolchain/pins.toml'), 'utf8')) as {
    include: { sharc: string[] };
  };
  return [
    path.join(moddir, 'build/gen'),
    path.join(moddir, 'src'),
    ...pins.include.sharc.map((d) => path.join(ROOT, d)),
  ];
}

/** Replace the `#include "name"` lines by the files' text, searched in `dirs`. */
export function expandIncludes(text: string, dirs: string[]): string {
  return patchkit(['sharc', 'inc', ...dirs.flatMap((d) => ['-I', d])], text);
}

/** `#ifdef NAME` / `#ifndef NAME` / `#else` / `#endif` lines (the build options). */
export function applyConditionals(text: string, defines: string[] = []): string {
  return patchkit(['sharc', 'cond', ...[...defines].sort().flatMap((d) => ['--define', d])], text);
}

const objects = new Map<string, SharcObject>();

/** Selache assembles `text` (an expanded source); the instruction check of the build applies. */
export function assemble(text: string): SharcObject {
  const key = createHash('sha256').update(text).digest('hex');
  const known = objects.get(key);
  if (known) return known;
  const j = JSON.parse(patchkit(['sharc', 'asm', '--text', '-'], text)) as {
    section: string;
    symbols: Record<string, [number, boolean]>;
    data: Record<string, [string, number, boolean]>;
    data_symbols: Record<string, [string, number, boolean]>;
  };
  const object: SharcObject = {
    text,
    parcels: j.section.length / 4,
    symbols: Object.fromEntries(Object.entries(j.symbols).map(([n, [p]]) => [n, p])),
    data: Object.fromEntries(
      Object.entries(j.data).map(([n, [hex, align, bss]]) => [
        n,
        { bytes: Uint8Array.from(Buffer.from(hex, 'hex')), bss, align },
      ]),
    ),
    dataSymbols: Object.fromEntries(
      Object.entries(j.data_symbols).map(([n, [section, offset]]) => [n, { section, offset }]),
    ),
  };
  objects.set(key, object);
  return object;
}

/** Place `object` at the short-word address `baseSw`, resolve its relocations against its own symbols and `externs`, and verify it. */
export function link(object: SharcObject, baseSw: number, externs: Record<string, number> = {}): Linked {
  const args = ['sharc', 'link', '--text', '-', '--base', `0x${baseSw.toString(16)}`];
  for (const [name, value] of Object.entries(externs)) args.push('--extern', `${name}=0x${value.toString(16)}`);
  const j = JSON.parse(patchkit(args, object.text)) as {
    base_sw: number;
    code_boot: string;
    symbols: Record<string, number>;
    checked?: number;
  };
  return { baseSw: j.base_sw, codeBoot: j.code_boot, symbols: j.symbols, checked: j.checked };
}

/** A DSP source as `pk_sharc` reads it: `#include` from the include directories and the source's own directory. */
export function assembleSource(file: string, includeDirs: string[]): SharcObject {
  return assemble(expandIncludes(readFileSync(file, 'utf8'), [...includeDirs, path.dirname(file)]));
}
