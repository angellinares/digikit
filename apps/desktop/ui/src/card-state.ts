// The refresh signal of the Files page and its card-folder strip: a Transfer job that ended bumps it, so the panel reads the folder at once (it also polls lightly).
import { createSignal } from 'solid-js';

export const [cardRefresh, setCardRefresh] = createSignal(0);
export const bumpCardRefresh = () => setCardRefresh((n) => n + 1);
