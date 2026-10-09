/**
 * Frustration signals: rage clicks, dead clicks and abandoned forms, sent as
 * `frustration` events named `rage_click`, `dead_click` and `form_abandon`.
 *
 * Turned on with `data-frustration` on the script tag. What each one means:
 *
 * - **Rage click**: {@link RAGE_CLICKS} or more clicks inside
 *   {@link RAGE_WINDOW_MS} within {@link RAGE_RADIUS_PX} pixels of each other.
 *   One report per burst. A triple click that selects text is how people
 *   select a paragraph, so a burst that leaves a text selection on something
 *   that is not a control is not counted.
 * - **Dead click**: a click on something that looks clickable (a link, a
 *   button, an element with a button role, an `onclick` or a `data-track`) that
 *   causes no navigation, no change to the page, and no network request within
 *   {@link DEAD_WINDOW_MS}. Controls that answer out of sight (a text field
 *   taking focus, a checkbox, a select, a video, a download link) are never
 *   candidates. One report per element per page view.
 * - **Abandoned form**: a form someone typed into or changed, left without
 *   being submitted (the page was closed or navigated away from). The report
 *   names the form (its `id`, `name`, or a short selector) and the LAST FIELD
 *   touched (its `name` or `id`). Never a value: nothing in this file reads one.
 *
 * Elements are named by a short CSS selector built from tag names, stable ids
 * and class names ({@link selectorOf}), never from the text on the page. A page
 * view sends at most {@link MAX_PER_PAGE} signals, so a broken page costs a
 * bounded number of hits.
 */
import type { Props, TrackerApi } from "../types.js";
import { cut, guard, plugin } from "./plugin.js";

export const RAGE_CLICKS = 3;
export const RAGE_WINDOW_MS = 1000;
export const RAGE_RADIUS_PX = 30;
export const DEAD_WINDOW_MS = 1000;
export const MAX_PER_PAGE = 20;

/** Elements a person expects something to happen on. */
const CLICKABLE =
  'a[href],button,summary,[role="button"],[role="link"],[role="tab"],[role="menuitem"],[onclick],[data-track],input[type="button"],input[type="submit"],input[type="image"],input[type="reset"]';

/** Controls that answer without a visible change the page can observe. */
const QUIET =
  'input:not([type="button"]):not([type="submit"]):not([type="image"]):not([type="reset"]),select,textarea,label,option,video,audio,[contenteditable],a[download]';

/** Form fields, for abandonment. */
const FIELD = "input,select,textarea";

/** An id or a class that a build tool or a framework generated, not a person. */
const GENERATED = /\d{3,}|\d[a-z]+\d|__|^[-_:]|^(css|sc|jsx)-|[^\w-]|.{31}/i;

/**
 * A short CSS selector for `el`: up to three levels, stopping at the first
 * stable `id` or `data-track` name, with at most two class names per level.
 * Generated ids and classes (`css-1x2y3z`, `Button__root__3kx`, `:r1:`) are
 * left out, because they change between builds and would split one element
 * into many rows.
 */
export function selectorOf(el: any): string {
  const parts: string[] = [];
  for (let e = el, depth = 0; e && e.tagName && depth < 3; e = e.parentElement, depth++) {
    const tag = e.tagName.toLowerCase();
    if (tag == "body" || tag == "html") break;
    const id = e.id,
      track = e.getAttribute && e.getAttribute("data-track");
    if (id && !GENERATED.test(id)) {
      parts.unshift(tag + "#" + id);
      break;
    }
    if (track) {
      parts.unshift(tag + '[data-track="' + cut(track, 40).replace(/"/g, "") + '"]');
      break;
    }
    const classes = [...(e.classList || [])].filter((c: string) => !GENERATED.test(c)).slice(0, 2);
    parts.unshift(tag + (classes.length ? "." + classes.join(".") : ""));
  }
  return cut(parts.join(" > "), 200);
}

/** The form's name for the report: its id, its name, or its selector. */
export const formName = (form: any): string =>
  cut(form.id || form.getAttribute("name") || selectorOf(form), 120);

/** The field's name for the report: its name, its id, or its type. */
export const fieldName = (field: any): string =>
  cut(field.getAttribute("name") || field.id || field.type || field.tagName.toLowerCase(), 120);

/**
 * True when the clicks in `recent` (newest last, each `[time, x, y]`) are a rage
 * burst: enough of them inside the window and the radius of the newest one.
 */
export function isRage(recent: [number, number, number][]): boolean {
  const last = recent[recent.length - 1];
  if (!last) return false;
  const near = recent.filter(
    ([at, x, y]) =>
      last[0] - at <= RAGE_WINDOW_MS && Math.hypot(x - last[1], y - last[2]) <= RAGE_RADIUS_PX,
  );
  return near.length >= RAGE_CLICKS;
}

plugin("frustration", (t: TrackerApi, w: any) => {
  const D = w.document,
    perf = w.performance;
  let page = "",
    sent = 0,
    reported: Record<string, 1> = {},
    clicks: [number, number, number][] = [],
    burst = -Infinity,
    // When something last answered: a DOM change, a request, a navigation.
    answered = 0,
    // Forms touched and not yet submitted, with the last field touched.
    forms = new Map<any, string>();

  const now = () => perf.now();

  /** Start a new page view when the path changes. */
  const pageView = () => {
    const here = w.location.pathname;
    if (here != page) {
      page = here;
      sent = 0;
      reported = {};
      clicks = [];
    }
  };

  const signal = (name: string, props: Props) => {
    pageView();
    if (sent >= MAX_PER_PAGE) return;
    sent++;
    t.x("frustration", { name: name, props: props, raw: true });
  };

  /* what counts as the page answering a click */

  const answer = guard(() => (answered = now()));
  try {
    new w.MutationObserver(
      guard((records: any[]) => {
        // A stylesheet or script landing in <head> is not an answer to anyone.
        if (records.some((r) => !(r.type == "childList" && D.head && D.head.contains(r.target))))
          answer();
      }),
    ).observe(D, { subtree: true, childList: true, attributes: true, characterData: true });
  } catch {}
  try {
    new w.PerformanceObserver(guard(answer)).observe({ type: "resource" });
  } catch {}
  // A request that starts but does not finish inside the window is an answer
  // too (a slow save is not a dead button), and the resource timeline only
  // lists finished requests. So the two ways a page starts one are wrapped to
  // note the moment, and otherwise left exactly as they were.
  const mark = (obj: any, key: string) => {
    const orig = obj && obj[key];
    if (typeof orig != "function") return;
    obj[key] = function (this: unknown, ...a: unknown[]) {
      answer();
      return orig.apply(this, a);
    };
  };
  mark(w, "fetch");
  mark(w.XMLHttpRequest && w.XMLHttpRequest.prototype, "send");
  for (const type of ["change", "input", "scroll", "toggle", "submit", "copy"])
    D.addEventListener(type, answer, true);
  w.addEventListener("blur", answer);

  /* clicks */

  D.addEventListener(
    "click",
    guard((ev: any) => {
      const target = ev.target;
      // `detail` is the click count: 1 or more for a pointer, 0 for a script's
      // `el.click()` and for a key pressed on a button. Neither of those is a
      // person failing to get a response from the page with a pointer.
      if (!ev.detail || !target || !target.closest) return;
      pageView();
      const at = now(),
        quiet = target.closest(QUIET),
        clickable = target.closest(CLICKABLE);

      // Rage: a burst in one spot.
      clicks = clicks.filter(([c]) => at - c <= RAGE_WINDOW_MS);
      clicks.push([at, ev.clientX || 0, ev.clientY || 0]);
      if (isRage(clicks) && at - burst > RAGE_WINDOW_MS && !quiet) {
        const selecting = !clickable && ("" + (w.getSelection && w.getSelection())).trim();
        if (!selecting) {
          burst = at;
          signal("rage_click", {
            selector: selectorOf(clickable || target),
            clicks: clicks.length,
          });
        }
      } else if (at - burst <= RAGE_WINDOW_MS) burst = at;

      // Dead: something clickable that nothing answers.
      if (!clickable || quiet || clickable.closest(QUIET)) return;
      const selector = selectorOf(clickable),
        href = w.location.href;
      if (reported[selector]) return;
      w.setTimeout(
        guard(() => {
          if (answered >= at || w.location.href != href || reported[selector]) return;
          // A link the browser is following is an answer, even before the
          // next page has begun to load.
          const link = clickable.closest("a[href]");
          if (link && !ev.defaultPrevented && !/^javascript:/i.test(link.getAttribute("href") || ""))
            return;
          reported[selector] = 1;
          signal("dead_click", { selector: selector });
        }),
        DEAD_WINDOW_MS,
      );
    }),
    true,
  );

  /* forms */

  const touch = guard((ev: any) => {
    const field = ev.target;
    if (!field || !field.matches || !field.matches(FIELD)) return;
    const form = field.form || field.closest("form");
    if (!form || /^(hidden|submit|button|reset|image)$/.test(field.type)) return;
    // Focus alone moves the last field of a form already in use; it does not
    // start one, because tabbing through a page is not filling in a form.
    if (ev.type != "focusin" || forms.has(form)) forms.set(form, fieldName(field));
  });
  for (const type of ["focusin", "input", "change"]) D.addEventListener(type, touch, true);
  D.addEventListener(
    "submit",
    guard((ev: any) => forms.delete(ev.target)),
    true,
  );

  /** Report every form still in use, then forget them: the page is going. */
  const leave = guard(() => {
    forms.forEach((field, form) => signal("form_abandon", { form: formName(form), field: field }));
    forms.clear();
  });
  w.addEventListener("pagehide", leave);
  // A client-side navigation to another path leaves the page too. A push that
  // only changes the query does not, and neither does `replaceState`: both are
  // how a search box keeps its query in the address bar while it is typed.
  const push = w.history && w.history.pushState;
  if (push)
    w.history.pushState = function (this: unknown, ...a: unknown[]) {
      try {
        if (a[2] != null && new URL("" + a[2], w.location.href).pathname != w.location.pathname)
          leave();
      } catch {}
      return push.apply(this, a);
    };
  w.addEventListener("popstate", leave);
});
