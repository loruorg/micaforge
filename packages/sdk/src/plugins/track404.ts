/**
 * `data-404`: a page that carries the `data-mf-404` marker reports one `error`
 * event named `404`. That is a broken inbound link, not an exception.
 *
 * A plugin rather than core code because it is opt-in and the marker is read
 * once, so it does not matter that this runs a moment after the page.
 */
import { plugin } from "./plugin.js";

plugin("track404", (t, w) => {
  if (w.document.querySelector("[data-mf-404]")) t.x("error", { name: "404" });
});
