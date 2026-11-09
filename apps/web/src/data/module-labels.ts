// Labels shared by the module cards. No manifest imports here, so a client island can use them.
import type { DeviceId } from './device-profiles';

export type Category = 'fx' | 'machines' | 'core' | 'test' | 'ui' | 'tools';

export const CATEGORY_LABEL: Record<Category, string> = {
  fx: 'Effect',
  machines: 'Machine',
  core: 'Framework',
  test: 'Test fixture',
  ui: 'Interface',
  tools: 'Tool',
};

export const DEVICE_SHORT: Record<DeviceId, string> = { dt2: 'DT2', dn2: 'DN2' };
