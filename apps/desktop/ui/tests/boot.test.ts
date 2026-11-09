import { describe, expect, it } from 'vitest';
import { booting, bootingScreen, bootingStatus, deviceFromName, loadFailure } from '../src/boot-core';

describe('loading state', () => {
  it('knows the device from the file name', () => {
    expect(deviceFromName('Digitakt_II_OS1.17.syx')).toBe('dt2');
    expect(deviceFromName('Digitone_II_OS1.12.syx')).toBe('dn2');
    expect(deviceFromName('image.syx')).toBeUndefined();
  });
  it('names the device, else the file, else nothing', () => {
    expect(bootingStatus(booting('Digitakt_II_OS1.17.syx'))).toBe('Booting Digitakt II…');
    expect(bootingStatus(booting('image.syx'))).toBe('Booting image.syx…');
    expect(bootingStatus(booting(''))).toBe('Booting…');
    expect(bootingScreen(booting('Digitakt_II_OS1.17.syx'))).toBe(
      'Booting Digitakt II… the first boot of a firmware can take a minute',
    );
  });
  it('puts the error in the failure line', () => {
    expect(loadFailure('a.syx', 'bad image')).toBe('Loading a.syx failed: bad image');
  });
});
