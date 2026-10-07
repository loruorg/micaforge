/**
 * The pen advance on static pages.
 *
 * A graphic marked `data-pen-write` is written left to right the first time it
 * scrolls into view: a trace, a ruling, a band of share ink. Words are never
 * marked; only what a pen draws moves.
 *
 * Content is never gated on this. Nothing is touched under reduced motion or
 * without IntersectionObserver, and a graphic already on screen when the page
 * loads is left exactly as it is: only one that is still below (or above) the
 * fold is held back, and it is held back by the same code that has already
 * arranged to write it in. Without this script every graphic simply stands.
 *
 * `data-pen-write="2"` delays the write by two steps, so a column of small
 * records is written one after another rather than all at once.
 */
const STEP_MS = 90;

export function armPens(root: ParentNode = document): void {
  if (typeof window === "undefined" || typeof IntersectionObserver === "undefined") return;
  if (window.matchMedia?.("(prefers-reduced-motion: reduce)").matches) return;

  // What is watched is the graphic's parent, never the graphic: a held graphic
  // is clipped to nothing, and Chromium measures intersection after the
  // element's own clip-path, so a held graphic would never be seen to arrive.
  const held = new Map<Element, Element[]>();
  const io = new IntersectionObserver(
    (entries) => {
      for (const entry of entries) {
        if (!entry.isIntersecting) continue;
        for (const el of held.get(entry.target) ?? []) el.setAttribute("data-pen", "write");
        held.delete(entry.target);
        io.unobserve(entry.target);
      }
    },
    { threshold: 0.35, rootMargin: "0px 0px -6% 0px" },
  );

  for (const el of root.querySelectorAll<HTMLElement | SVGElement>("[data-pen-write]")) {
    const box = el.getBoundingClientRect();
    if (box.bottom > 0 && box.top < window.innerHeight) continue;
    const order = Number(el.getAttribute("data-pen-write"));
    if (Number.isFinite(order) && order > 0) {
      el.style.setProperty("--mf-pen-delay", `${order * STEP_MS}ms`);
    }
    const watch = el.parentElement ?? el;
    held.set(watch, [...(held.get(watch) ?? []), el]);
    el.setAttribute("data-pen", "pending");
    io.observe(watch);
  }
}
