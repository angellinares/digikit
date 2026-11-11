// The stroke icons of the workbench buttons (16x16 paths), shared by Console.tsx and the UI preview.
export function Icon(props: { d: string }) {
  return (
    <svg viewBox="0 0 16 16" aria-hidden="true">
      <path
        d={props.d}
        fill="none"
        stroke="currentColor"
        stroke-width="1.6"
        stroke-linecap="round"
        stroke-linejoin="round"
      />
    </svg>
  );
}
export const FOLDER = 'M1.5 4V12.5H14.5V5.5H7.5L6 4Z';
export const RELOAD = 'M13 3V6.5H9.5M13 6.5A5.2 5.2 0 1 0 13.4 10';
export const PAUSE = 'M5.5 3V13M10.5 3V13';
export const PLAY = 'M4.5 2.5L12.5 8L4.5 13.5Z';
export const RESTART = 'M3 2.5V6H6.5M3 6A5.2 5.2 0 1 1 2.8 9.8';
export const CHART = 'M2 12.5L5.5 7.5L8.5 10L13.5 3.5M10.5 3.5H13.5V6.5';
export const SPEAKER = 'M2.5 6H5L8.5 3V13L5 10H2.5ZM11 5.5a3.5 3.5 0 0 1 0 5';
export const WARNING = 'M8 2.5L14.5 13.5H1.5ZM8 6.5V9.5M8 11.5V11.6';
export const UP = 'M4 10L8 6L12 10';
export const REMOTE = 'M2.5 3.5H13.5V10.5H2.5ZM6 13H10M8 10.5V13';
// a chip with pins
export const FIRMWARE =
  'M4.5 4.5H11.5V11.5H4.5ZM6.5 2.5V4.5M9.5 2.5V4.5M6.5 11.5V13.5M9.5 11.5V13.5M2.5 6.5H4.5M2.5 9.5H4.5M11.5 6.5H13.5M11.5 9.5H13.5';
export const MODULES = 'M2.5 5L8 2.5L13.5 5V11L8 13.5L2.5 11ZM2.5 5L8 7.5L13.5 5M8 7.5V13.5';
export const DOWN = 'M4 6L8 10L12 6';
