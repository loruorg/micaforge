/**
 * Click tags: a click on any element carrying `data-track="name"` sends a
 * custom event with that name, and its `data-track-*` attributes become the
 * properties, so `<button data-track="signup" data-track-plan="pro">` sends
 * `signup` with `{ plan: "pro" }`. Tagging a button needs no script of your own.
 *
 * Turned on with `data-tags` on the script tag. The values are the attribute
 * text as written; nothing is read from the element's content or from a form
 * field. A click on a child of a tagged element counts for the nearest tagged
 * ancestor. Clicks in the moment before this file arrives are not seen, which
 * is the price of keeping it out of the 3 KB core.
 */
import type { Props } from "../types.js";
import { cut, guard, plugin } from "./plugin.js";

/** The custom event a tagged element stands for, or `null` for an untagged one. */
export const tagEvent = (el: any): { name: string; props: Props } | null => {
  const tag = el && el.closest && el.closest("[data-track]");
  if (!tag) return null;
  const props: Props = {};
  for (const at of tag.attributes)
    if (!at.name.indexOf("data-track-")) props[at.name.slice(11)] = cut(at.value, 200);
  return { name: cut(tag.getAttribute("data-track"), 200), props: props };
};

plugin("tags", (t, w) => {
  w.document.addEventListener(
    "click",
    guard((ev: any) => {
      const e = tagEvent(ev.target);
      // A custom event like any `track()` call: the page's default properties
      // are merged in, so a tag reads exactly like the call it replaces.
      if (e && e.name) t.track(e.name, e.props);
    }),
    true,
  );
});
