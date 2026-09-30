// Direct imports preserve every authored faceplate field from the repository TOML.
// Vendored byte-for-byte from the faceplate source at commit 858140f.
import { parse } from 'smol-toml';
import dt2Source from './faceplates/digitakt-ii.toml?raw';
import dn2Source from './faceplates/digitone-ii.toml?raw';

export type Key = { code: number; x: number; y: number; w: number; h: number; legend?: string; sub?: string; sub_y?: number; sub_style?: string; icon?: string; style?: string };
export type Knob = { encoder?: number; push?: number; x: number; y: number; r: number; marker?: string; label: string; label_y?: number; sub?: string };
export type Text = { text: string; x: number; y: number; style: string };
export type Panel = { name: string; accent: string; func_legend: string; bezel: number[]; glass: number[]; wordmark: number[]; leds: { x: number[]; y: number[]; r: number }; screws: { x: number; y: number }[]; keys: Key[]; knobs: Knob[]; texts: Text[] };
type Source = { faceplate: { wordmark: string; accent: string; func_legend: string }; display: { bezel: number[]; glass: number[]; wordmark: number[] }; leds: Panel['leds']; controls: { screws: Panel['screws']; keys: Key[]; knobs: Knob[]; texts: Text[] } };
function panel(name: string, source: string): Panel {
  const data = parse(source) as unknown as Source;
  return { name, accent: data.faceplate.accent, func_legend: data.faceplate.func_legend, bezel: data.display.bezel, glass: data.display.glass, wordmark: data.display.wordmark, leds: data.leds, ...data.controls };
}
export const panels = { dt2: panel('Digitakt II', dt2Source), dn2: panel('Digitone II', dn2Source) } as const;
