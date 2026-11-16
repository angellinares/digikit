// The Transfer page of the site: asks for Web MIDI access (SysEx), picks the first device port, then shows the shared pane (TransferPage.tsx).
// Without Web MIDI (Safari) or without the SysEx permission the pane still opens on the `none` target; its error line says why (web-env.ts, `whyNone`)
// and its Try again button asks again. The island builds the site's app once and gives it to the pane (AppProvider).
import { Show, createSignal, onMount, type JSX } from 'solid-js';
import AppProvider from '@digi/workbench-core/ui/transfer/AppProvider.tsx';
import TransferPage from '@digi/workbench-core/ui/transfer/TransferPage.tsx';
import { createWebTransferApp } from './web-env';

export default function WebTransfer(): JSX.Element {
  const web = createWebTransferApp();
  const [ready, setReady] = createSignal(false);
  onMount(() => {
    // /transfer/?add=<ids>: modules to tick once the list is read
    const add = new URLSearchParams(location.search).get('add');
    if (add)
      web.app.state.setPendingAdd(
        add
          .split(',')
          .map((s) => s.trim())
          .filter(Boolean),
      );
    // a failure leaves the target on none: the pane shows the reason
    void web
      .start()
      .catch(() => {})
      .finally(() => setReady(true));
  });
  return (
    <Show when={ready()}>
      <AppProvider app={web.app}>
        <TransferPage
          onFlash={() => {
            location.href = `${import.meta.env.BASE_URL.replace(/\/?$/, '/')}firmware/`;
          }}
        />
      </AppProvider>
    </Show>
  );
}
