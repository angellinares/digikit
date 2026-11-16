// The desktop workbench's apps: the Transfer app (target, connection, jobs, page state) and the Firmware app on it, built once at startup from
// the Host (host/host.ts) and the webview's storage. main.tsx gives them to the pages (packages/workbench-core/ui/transfer/AppProvider.tsx).
import { DkSession } from '@digi/dk-session';
import { indexedDbBlobStore, type BlobStore } from '@digi/workbench-core/blob-store.ts';
import { createFirmwareApp, type FirmwareApp } from '@digi/workbench-core/firmware/app.ts';
import { localStorageKeyValue, type KeyValue } from '@digi/workbench-core/storage.ts';
import { createTransferApp, type TransferApp } from '@digi/workbench-core/transfer/app.ts';
import { createSessionHub } from '@digi/workbench-core/transfer/session-core.ts';
import { createDesktopEnv } from './desktop-env';
import { createDesktopFirmwareEnv } from './desktop-firmware';
import type { Host } from '@digi/workbench-core/host/host.ts';

export interface DesktopApps {
  app: TransferApp;
  firmware: FirmwareApp;
}

/** The webview's keep of the user's stock firmware (for binding services; it never leaves this machine). */
const stockBlobs = (): BlobStore => indexedDbBlobStore('digikit-stock', 'firmware');

export function createDesktopApps(
  host: Host,
  storage: KeyValue = localStorageKeyValue(),
  stock: BlobStore = stockBlobs(),
): DesktopApps {
  let app!: TransferApp;
  const getApp = () => app;
  app = createTransferApp(createDesktopEnv(host, getApp), {
    storage,
    stockBlobs: stock,
    sessions: createSessionHub((transport) => new DkSession(transport, { timeoutMs: 6000 })),
  });
  return { app, firmware: createFirmwareApp(createDesktopFirmwareEnv(host, getApp), app) };
}
