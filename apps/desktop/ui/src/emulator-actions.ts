// Actions of the Emulator page that other pages offer (Console.tsx registers them; desktop-env.ts hands them to the shared pages).
let reloadWith: (() => void) | undefined;
export const setReloadWithRuntime = (f: () => void): void => {
  reloadWith = f;
};
/** Tick "Boot with runtime" and reload the emulator. */
export const reloadWithRuntime = (): void => reloadWith?.();
