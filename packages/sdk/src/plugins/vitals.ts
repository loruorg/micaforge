/**
 * Core Web Vitals: LCP, CLS, INP, FCP and TTFB, read straight off
 * `PerformanceObserver` with no library and no polyfill.
 *
 * On by default (`data-vitals="false"` turns it off). It lived in the core until
 * the core needed the bytes for sampling and URL masking; it moved here because
 * it is the one default feature that loses almost nothing by arriving a moment
 * after the page. Every observer is registered with `buffered: true`, so the
 * largest paint and the layout shifts that happened before this file arrived
 * are read back from the browser's own buffer. The one cost: interactions under
 * 104 ms that happened before it loaded are not in that buffer, so a very fast
 * early tap does not count towards INP.
 *
 * The readings ride out once per document, when the page is hidden or
 * `flush()` is called: the core calls `h` at exactly those moments.
 */
import type { Vitals } from "../types.js";
import { guard, plugin } from "./plugin.js";

plugin("vitals", (t, w) => {
  const R = Math.round,
    v: Vitals = {};
  let sent = 0;

  const observe = (type: string, cb: (e: any) => void, extra?: object) => {
    try {
      new w.PerformanceObserver((list: any) => list.getEntries().forEach(guard(cb))).observe({
        type: type,
        buffered: true,
        ...extra,
      });
    } catch {}
  };

  observe("largest-contentful-paint", (e) => (v.lcp = R(e.renderTime || e.startTime)));
  observe(
    "event",
    (e) => {
      if (e.interactionId && e.duration > (v.inp || 0)) v.inp = R(e.duration);
    },
    { durationThreshold: 40 },
  );
  // CLS as the worst 5 s session window, the way the metric is defined.
  let val = 0,
    first = 0,
    last = 0;
  observe("layout-shift", (e) => {
    if (e.hadRecentInput) return;
    if (val && e.startTime - last < 1000 && e.startTime - first < 5000) {
      val += e.value;
      last = e.startTime;
    } else {
      val = e.value;
      first = last = e.startTime;
    }
    if (val > (v.cls || 0)) v.cls = val;
  });

  const prev = t.h;
  t.h = guard(() => {
    if (prev) prev();
    if (sent) return;
    sent = 1;
    // FCP and TTFB are already on the timeline, so neither needs an observer.
    try {
      const perf = w.performance,
        nav = perf.getEntriesByType("navigation")[0],
        fcp = perf.getEntriesByName("first-contentful-paint")[0];
      if (nav) v.ttfb = R(nav.responseStart);
      if (fcp) v.fcp = R(fcp.startTime);
    } catch {}
    if (Object.keys(v).length) t.x("web_vitals", { vitals: v });
  });
});
