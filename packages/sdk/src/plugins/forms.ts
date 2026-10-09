/**
 * `data-forms`: one `form` event per submission, named by the form's `id` or
 * `name`. Field values never leave the page; the form is not even read beyond
 * those two attributes.
 *
 * A plugin rather than core code because it is opt-in and loses nothing by
 * loading a moment after the page: a form is submitted after it is typed into.
 */
import { guard, plugin } from "./plugin.js";

plugin("forms", (t, w) => {
  w.document.addEventListener(
    "submit",
    guard((ev: any) => t.x("form", { name: ev.target.id || ev.target.name || "" })),
    true,
  );
});
