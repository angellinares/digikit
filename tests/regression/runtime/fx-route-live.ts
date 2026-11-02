import type { Device } from '@digi/test-support';
import { withPreparedFxSession, type PreparedFxSessionOptions } from './fx-session.ts';
import { FX, GAIN, RT as RUNTIME_ROOTS, SEND, SINE } from './fixtures.ts';

const RT = [...RUNTIME_ROOTS, 'core/dynfx'];
export const SENDONLY = 'test/dyn-fx-gain-sendonly';
export const SEND3 = `${SEND}-3`;

export interface RouteFxSessionOptions extends Omit<PreparedFxSessionOptions, 'image'> {
  image: 'routes' | 'track' | 'track-base';
}

/** Exact legacy image, inventory and startup-write policy for each route family. */
export function withRouteFxSession(device: Device, scope: string, config: RouteFxSessionOptions) {
  return withPreparedFxSession(device, scope, {
    ...config,
    image: {
      name: config.image,
      modules: [
        ...RT,
        ...(config.image === 'track-base' ? [] : ['core/fxpages']),
        ...(config.image === 'routes' ? ['core/digikit-menu'] : []),
      ],
      inventory: config.image === 'routes' ? [SINE, GAIN, SEND, SENDONLY, SEND3] : [SINE, FX, `${FX}-2`, `${FX}-3`],
      symbols: [
        'DKRT_BOOT',
        'DKRT_ASSIGN',
        'fx.rows',
        '_dl_area',
        ...(config.image === 'track' || config.image === 'track-base' ? ['dynreg_info'] : []),
        ...(config.image === 'track'
          ? ['fxpages_info', 'fxp_send_row', 'fxp_send_count', 'fxp_nload', 'DKRT_FXS']
          : []),
        ...(config.image === 'routes'
          ? [
              'DK_FXR_PEND',
              'DK_FXR_SET',
              'DK_FXR_FLUSH',
              'DK_POPUP',
              'DK_POPUP_TEXT',
              'DK_POPUPS',
              'DK_APPLIES',
              'DK_PANEL',
              'DK_PANEL_N',
              'DK_ROUTE_N',
              'DK_ROUTE_ITEM',
              'fxp_send_count',
              'DKRT_FXS',
              'fxpages_info',
            ]
          : []),
      ],
      policy: 'fx-routes',
    },
  });
}
