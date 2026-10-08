/**
 * The homepage's motion: the preview's chart drawing in, its figures counting
 * up, its live feed ticking, and the three feature visuals building when they
 * scroll into view.
 *
 * Everything is complete in the markup first. Nothing here runs under reduced
 * motion, and an element is only held in its starting pose (`data-armed`) when
 * this script is about to release it.
 */

type LiveEvent = { kind: "people" | "ai-referral" | "crawler"; who: string; page: string };

const FEED_MS = 2600;
const COUNT_MS = 1100;

const nf = new Intl.NumberFormat("en-US");

function onceVisible(el: Element, threshold: number, run: () => void) {
  const io = new IntersectionObserver(
    (entries) => {
      if (entries.some((e) => e.isIntersecting)) {
        io.disconnect();
        run();
      }
    },
    { threshold },
  );
  io.observe(el);
}

function countUp(el: HTMLElement) {
  const target = Number(el.dataset.count);
  if (!Number.isFinite(target)) return;
  const started = performance.now();
  const tick = (now: number) => {
    const t = Math.min(1, (now - started) / COUNT_MS);
    const eased = 1 - Math.pow(2, -10 * t);
    el.textContent = nf.format(Math.round(target * (t === 1 ? 1 : eased)));
    if (t < 1) requestAnimationFrame(tick);
  };
  requestAnimationFrame(tick);
}

function eventNode(e: LiveEvent): HTMLLIElement {
  const li = document.createElement("li");
  li.className = "hp__event is-new";
  li.dataset.kind = e.kind;
  const mark = document.createElement("span");
  mark.className = "hp__event-mark";
  mark.setAttribute("aria-hidden", "true");
  const text = document.createElement("span");
  text.className = "hp__event-text";
  const who = document.createElement("strong");
  who.textContent = e.who;
  const verb = document.createElement("span");
  verb.textContent = e.kind === "crawler" ? "crawled" : "sent a visitor to";
  text.append(who, verb);
  const page = document.createElement("span");
  page.className = "hp__event-page";
  page.textContent = e.page;
  li.append(mark, text, page);
  return li;
}

function startFeed(feed: HTMLElement, events: LiveEvent[]) {
  const counter = document.querySelector<HTMLElement>("[data-live-count]");
  let next = feed.children.length % events.length;
  let visible = false;
  let timer = 0;

  const step = () => {
    const e = events[next];
    next = (next + 1) % events.length;
    feed.prepend(eventNode(e));
    while (feed.children.length > 6) feed.lastElementChild?.remove();
    if (counter) {
      const now = Number(counter.textContent) || 37;
      const drift = Math.round((Math.random() - 0.45) * 4);
      counter.textContent = String(Math.min(48, Math.max(29, now + drift)));
    }
  };

  const sync = () => {
    const run = visible && document.visibilityState === "visible";
    if (run && !timer) timer = window.setInterval(step, FEED_MS);
    if (!run && timer) {
      window.clearInterval(timer);
      timer = 0;
    }
  };

  new IntersectionObserver((entries) => {
    visible = entries.some((e) => e.isIntersecting);
    sync();
  }).observe(feed);
  document.addEventListener("visibilitychange", sync);
}

export function startHomeMotion() {
  if (window.matchMedia("(prefers-reduced-motion: reduce)").matches) return;

  const preview = document.querySelector<HTMLElement>(".hp");
  if (preview) {
    const svg = preview.querySelector("[data-draw]");
    const counts = preview.querySelectorAll<HTMLElement>("[data-count]");
    const rows = preview.querySelector<HTMLElement>(".hp__lower");
    rows?.setAttribute("data-armed", "");
    onceVisible(preview, 0.15, () => {
      svg?.classList.add("is-drawing");
      counts.forEach(countUp);
      requestAnimationFrame(() => {
        rows?.classList.add("is-in");
        rows?.removeAttribute("data-armed");
      });
    });

    const feed = preview.querySelector<HTMLElement>("[data-feed]");
    const data = document.getElementById("hp-live")?.textContent;
    if (feed && data) {
      try {
        startFeed(feed, JSON.parse(data) as LiveEvent[]);
      } catch {
        // A malformed demo list only means the feed stays still.
      }
    }
  }

  document.querySelectorAll<HTMLElement>("[data-reveal]").forEach((el) => {
    const box = el.getBoundingClientRect();
    // Already on screen at load: leave it as it is rather than blank it and replay.
    if (box.top < window.innerHeight * 0.9) return;
    el.setAttribute("data-armed", "");
    onceVisible(el, 0.35, () => {
      el.classList.add("is-in");
      el.removeAttribute("data-armed");
    });
  });
}
