// The Tauri window on macOS shows no `title` tooltip. This shows an element's title in a small box after a short hover or on keyboard
// focus, kept inside the window. While the box shows, the title moves to `data-title` so a browser does not show its own as well.
const DELAY_MS = 450;

export function installTooltips(): void {
  const tip = document.createElement('div');
  tip.className = 'tip';
  tip.setAttribute('role', 'tooltip');
  tip.hidden = true;
  document.body.appendChild(tip);

  let timer = 0;
  let watch = 0;
  let owner: HTMLElement | null = null;

  const find = (n: EventTarget | null) =>
    n instanceof Element ? (n.closest('[title], [data-title]') as HTMLElement | null) : null;

  const show = (el: HTMLElement) => {
    const text = el.getAttribute('title') ?? el.getAttribute('data-title');
    if (!text) return;
    owner = el;
    if (el.hasAttribute('title')) {
      el.setAttribute('data-title', text);
      el.removeAttribute('title');
    }
    tip.textContent = text;
    tip.hidden = false;
    const r = el.getBoundingClientRect();
    const t = tip.getBoundingClientRect();
    const left = Math.max(8, Math.min(r.left + r.width / 2 - t.width / 2, window.innerWidth - t.width - 8));
    let top = r.bottom + 6;
    if (top + t.height > window.innerHeight - 8) top = r.top - t.height - 6;
    tip.style.left = `${left}px`;
    tip.style.top = `${top}px`;
    // an owner removed from the page while its tooltip shows: hide it
    window.clearInterval(watch);
    watch = window.setInterval(() => {
      if (owner && !owner.isConnected) hide();
    }, 400);
  };

  const hide = () => {
    window.clearTimeout(timer);
    window.clearInterval(watch);
    if (owner) {
      if (!owner.hasAttribute('title') && owner.hasAttribute('data-title')) {
        owner.setAttribute('title', owner.getAttribute('data-title')!);
        owner.removeAttribute('data-title');
      } else {
        // a re-render set a new title meanwhile
        owner.removeAttribute('data-title');
      }
    }
    owner = null;
    tip.hidden = true;
  };

  document.addEventListener('pointerover', (e) => {
    const el = find(e.target);
    if (!el || el === owner) return;
    hide();
    timer = window.setTimeout(() => show(el), DELAY_MS);
  });
  document.addEventListener('pointerout', (e) => {
    const to = (e as PointerEvent).relatedTarget;
    const from = find(e.target) ?? owner;
    if (from && to instanceof Node && from.contains(to)) return;
    hide();
  });
  document.addEventListener('pointerdown', hide);
  document.addEventListener('keydown', (e) => {
    if (e.key === 'Escape') hide();
  });
  document.addEventListener('focusin', (e) => {
    const el = find(e.target);
    if (el && (e.target as Element).matches(':focus-visible')) {
      hide();
      show(el);
    }
  });
  document.addEventListener('focusout', hide);
  // only a shown tooltip, and only a scroll of an element that holds its owner (the log following new lines must not cancel a pending one)
  document.addEventListener(
    'scroll',
    (e) => {
      if (owner && !tip.hidden && e.target instanceof Node && (e.target === document || e.target.contains(owner)))
        hide();
    },
    { capture: true, passive: true },
  );
}
