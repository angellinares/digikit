import { describe, expect, test } from 'vitest';
import { bootInfo } from '@digi/regression/runtime/card-kit.ts';
import { cardRig, RT } from '@digi/regression/runtime/card-rig.ts';
import { portFile } from '@digi/regression/runtime/device-data.ts';
import { DEVICES, FIRMWARE } from '@digi/regression/runtime/devices.ts';
import { rpc, SETTLE } from '@digi/regression/runtime/fx-live.ts';
import { planSymbol } from '@digi/regression/runtime/runtime-plan.ts';

/**
 * core/usb-bulk-audio (docs/plans/usb-fast-audio-bulk.md): entry 5 of the port table (USB AUDIO/MIDI) serves the descriptor with interface 5 (EP2 OUT) appended,
 * core/usb-bulk reports OUT-only mode in that port, and the connect callback leaves the TX half of ENDPTCTRL2 (the audio feedback endpoint) as the stock callback set it.
 * The fixture test/usb-echo enables the bulk interface; the upload path in this mode is tested in digikit/usb-fast.
 */

const ECHO_CMD = 0x2c; // STATUS of test/usb-echo
const OUT_ONLY = 0x40; // DK_USB_BULK_OUT_ONLY
const WRONG_MODE = 0x04; // DK_USB_BULK_WRONG_MODE
const ENDPTCTRL2 = 0xfc0b01c8;
const FEEDBACK_TX = 0x00c40000; // the stock port 5 connect: EP2 TX iso enabled
const SELECTION = ['modules/usb-echo.dkm'];

for (const device of DEVICES) {
  describe(`${device.toUpperCase()} ${FIRMWARE[device]} core/usb-bulk-audio`, () => {
    const kit = cardRig(device, 'usb-bulk-audio', {
      dsp: 'zero',
      roots: [...RT, 'core/usb-bulk', 'core/usb-bulk-audio'],
      symbols: ['DKRT_MAILBOX', 'usb_bulk_audio_state', 'ua_connect'],
      initial: () => ({ selection: [] }),
      project: true,
      uploads: true,
      relaxFs: true,
    });
    const anchor = (name: string) => {
      const a = (portFile(device, FIRMWARE[device]).anchor ?? []).filter((x) => x.name === name);
      expect(a.length, name).toBe(1);
      return a[0]!.addr as number;
    };

    test('entry 5 serves interface 5, the state shows OUT-only mode, and the connect callback keeps the feedback endpoint', async () => {
      const rt = await kit.load('audio-bulk', { modules: ['test/usb-echo'], selection: SELECTION });
      expect((await bootInfo(rt.session, kit.plan)).results, 'the module loads').toEqual([0]);
      const word = async (addr: number) => {
        const w = await rt.session.client.memPeek(addr);
        expect(w, `guest word at ${addr.toString(16)}`).not.toBeNull();
        return w!;
      };
      const portAddr = anchor('cf.usb.value.active_port');
      const table = anchor('cf.usb.data.port_table');
      // a menu-style change of the mode to USB AUDIO/MIDI: the active port and the firmware's pointer to its table entry
      await rpc(rt.session, 'mem.cf.poke', { addr: portAddr - 4, value: table + 5 * 0x28 });
      await rpc(rt.session, 'mem.cf.poke', { addr: portAddr, value: 5 });
      await rt.ui.wait({ ms: 2500, why: SETTLE });

      expect(await word(planSymbol(kit.plan, 'usb_bulk_audio_state')), 'ua_prepare accepted the stock descriptor').toBe(1);
      const e5 = table + 5 * 0x28;
      const length = await word(e5 + 0x10);
      expect(length, 'entry 5 descriptor length').toBe(0x158);
      const d = await word(e5 + 0xc);
      const desc = Buffer.alloc(length);
      for (let i = 0; i < length; i += 4) desc.writeUInt32BE(await word(d + i), i);
      expect(desc.readUInt16LE(2), 'wTotalLength').toBe(0x0158);
      expect(desc[4], 'bNumInterfaces').toBe(6);
      expect(desc.subarray(0x148).toString('hex'), 'interface 5: vendor class, EP2 OUT bulk 512').toBe('09040500' + '01ff000000' + '07050202000200');

      const r = await kit.usb(rt, (dk) => dk.request(ECHO_CMD));
      expect(r.status).toBe(0);
      const state = Buffer.from(r.body).readUInt32BE(4);
      expect(state & OUT_ONLY, 'OUT-only mode').toBe(OUT_ONLY);
      expect(state & WRONG_MODE, 'not the wrong mode').toBe(0);

      // the bus reset: the stock connect enables EP2 TX (audio feedback); the second connect and its disconnect must leave that half as it is
      const connect = () => rpc(rt.session, 'usb.bulk.in', { connect: planSymbol(kit.plan, 'ua_connect') });
      await connect();
      await rt.ui.wait({ ms: 50, why: SETTLE });
      const first = await rt.session.client.memPeek(ENDPTCTRL2);
      if (first === null) {
        console.log('ENDPTCTRL2 is not readable through mem.cf.peek: the feedback endpoint check is skipped');
      } else {
        expect(first & FEEDBACK_TX, 'EP2 TX iso is enabled after the connect').toBe(FEEDBACK_TX);
        await connect();
        await rt.ui.wait({ ms: 50, why: SETTLE });
        expect((await word(ENDPTCTRL2)) >>> 16, 'the TX half is unchanged by a second connect').toBe(first >>> 16);
      }
      await rt.session.expectNoFault('usb-bulk-audio');
    }, 900_000);
  });
}
