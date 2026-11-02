import type { Device } from '@digi/test-support';

/** Module ids of the test fixtures and the runtime root modules shared by the FX scenarios. */
export const SINE = 'test/sine';
export const GAIN = 'test/dyn-fx-gain';
export const SEND = 'test/dyn-fx-gain-send';
export const FX = 'test/dyn-fx-gain-both';
export const RT = ['core/runtime', 'core/dynreg', 'core/dyndsp'];

/** The absolute DSP frame of the note trigger that every FX scenario shares per device. */
export const triggerFrame = (device: Device): number => (device === 'dt2' ? 7500 : 7000);
