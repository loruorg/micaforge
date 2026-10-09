// @vitest-environment jsdom
/**
 * Plugin tests.
 *
 * These run in jsdom, unlike the core's tests: a plugin's whole job is to read
 * the DOM (closest(), attributes, forms, mutations), and a hand-built stub of all
 * that would test the stub. The core is still jsdom-free, so its surface stays
 * pinned to exactly the globals it touches.
 *
 * Each test installs a plugin against a fake tracker that records what the
 * plugin hands to `x`, which is the plugin's entire output.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { selectorOf, isRage, formName, fieldName } from "./frustration.js";
import { toReport, scrub, MAX_PER_PAGE, MAX_FRAMES } from "./errors.js";
import { tagEvent } from "./tags.js";

interface Fake {
  x: ReturnType<typeof vi.fn>;
  track: ReturnType<typeof vi.fn>;
  h?: () => void;
}

let t: Fake;

/** Load a plugin file afresh against a new fake tracker. */
async function install(name: string): Promise<Fake> {
  t = { x: vi.fn(), track: vi.fn() };
  (globalThis as any).__mf = t;
  vi.resetModules();
  await import(`./${name}.js`);
  return t;
}

/** A pointer click: `detail` 1, which a script's `el.click()` never carries. */
const click = (el: Element, x = 10, y = 10) =>
  el.dispatchEvent(
    new MouseEvent("click", { bubbles: true, cancelable: true, clientX: x, clientY: y, detail: 1 }),
  );

/** Let MutationObserver callbacks (microtasks) run. */
const settle = () => new Promise((r) => setTimeout(r, 0));

beforeEach(() => {
  document.body.innerHTML = "";
  document.head.innerHTML = "";
  window.history.replaceState({}, "", "/start");
});

afterEach(() => {
  vi.useRealTimers();
  delete (globalThis as any).__mf;
});

describe("the plugin loader contract", () => {
  it("does nothing without a tracker, and installs once", async () => {
    delete (globalThis as any).__mf;
    vi.resetModules();
    await expect(import("./forms.js")).resolves.toBeDefined();

    const fake = await install("forms");
    vi.resetModules();
    await import("./forms.js"); // the same file loaded twice
    document.body.innerHTML = `<form id="f"></form>`;
    document.getElementById("f")!.dispatchEvent(new Event("submit", { bubbles: true }));
    expect(fake.x).toHaveBeenCalledTimes(1);
  });
});

describe("errors", () => {
  it("turns an error event into a report, cut, scrubbed and free of query strings", () => {
    const stack = ["TypeError: x is not a function"]
      .concat(Array.from({ length: 50 }, (_, i) => `    at f${i} (https://example.com/app.js?v=9&token=abc:1:${i})`))
      .join("\n");
    const err = Object.assign(new TypeError("cannot read a@example.com"), { stack });
    const r = toReport({
      message: "Uncaught TypeError: cannot read a@example.com",
      filename: "https://example.com/app.js?token=abc",
      lineno: 12,
      colno: 7,
      error: err,
    })!;
    expect(r.name).toBe("TypeError");
    expect(r.message).toBe("Uncaught TypeError: cannot read [email]");
    expect(r.filename).toBe("https://example.com/app.js");
    expect(r.lineno).toBe(12);
    expect(r.colno).toBe(7);
    expect(r.stack.split("\n")).toHaveLength(MAX_FRAMES);
    expect(r.stack).not.toContain("token");
    expect(r.stack).not.toContain("a@example.com");
  });

  it("drops what a site cannot act on", () => {
    expect(toReport({ message: "Script error.", filename: "" })).toBeNull();
    expect(
      toReport({ message: "boom", filename: "chrome-extension://abcdef/content.js", error: new Error("boom") }),
    ).toBeNull();
    expect(toReport({ message: "ResizeObserver loop completed with undelivered notifications." })).toBeNull();
    // A cross-origin script error that does carry a file is still reported.
    expect(toReport({ message: "Script error.", filename: "https://cdn.example.com/a.js" })).not.toBeNull();
  });

  it("reads every shape a rejection reason arrives in", () => {
    expect(toReport({ error: new RangeError("bad range") }, true)).toMatchObject({
      name: "RangeError",
      message: "bad range",
    });
    expect(toReport({ error: "plain string" }, true)).toMatchObject({
      name: "UnhandledRejection",
      message: "plain string",
    });
    expect(toReport({ error: { code: 42 } }, true)!.message).toBe('{"code":42}');
    expect(toReport({ error: { message: "from an object" } }, true)!.message).toBe("from an object");
    const circular: any = {};
    circular.self = circular;
    expect(toReport({ error: circular }, true)!.message).toBe("[object Object]");
  });

  it("scrubs emails and query strings out of free text", () => {
    expect(scrub("see https://x.com/a?b=c and me@mail.co.uk")).toBe("see https://x.com/a and [email]");
  });

  it("reports an uncaught error once per page view, with the release, unhandled", async () => {
    document.head.innerHTML = `<script data-site="1" data-release="2026.10.1"></script>`;
    const fake = await install("errors");
    const fire = (message: string) =>
      window.dispatchEvent(
        new ErrorEvent("error", { message, filename: "https://example.com/app.js", lineno: 1, colno: 2, error: new Error(message) }),
      );
    fire("first");
    fire("first");
    fire("second");
    expect(fake.x).toHaveBeenCalledTimes(2);
    const [kind, ev] = fake.x.mock.calls[0];
    expect(kind).toBe("error");
    expect(ev).toMatchObject({
      name: "Error",
      raw: true,
      props: { message: "first", handled: false, release: "2026.10.1", lineno: 1, colno: 2 },
    });

    // A new page view: the same error counts again there.
    window.history.pushState({}, "", "/next");
    fire("first");
    expect(fake.x).toHaveBeenCalledTimes(3);
  });

  it("stops at the per-page limit", async () => {
    const fake = await install("errors");
    for (let i = 0; i < MAX_PER_PAGE + 5; i++)
      window.dispatchEvent(new ErrorEvent("error", { message: "e" + i, error: new Error("e" + i) }));
    expect(fake.x).toHaveBeenCalledTimes(MAX_PER_PAGE);
  });

  it("reports an unhandled rejection", async () => {
    const fake = await install("errors");
    const ev = new Event("unhandledrejection") as any;
    ev.reason = new Error("nope");
    window.dispatchEvent(ev);
    expect(fake.x.mock.calls[0][1]).toMatchObject({ name: "Error", props: { message: "nope" } });
  });
});

describe("frustration: naming elements", () => {
  it("builds a short selector from stable ids and classes only", () => {
    document.body.innerHTML = `
      <section id="pricing"><div class="card css-1x2y3z"><button class="buy primary large">Buy</button></div></section>
      <div id=":r1:"><span class="Button__root__3kx">x</span></div>
      <a data-track="signup" href="/s"><em>go</em></a>`;
    expect(selectorOf(document.querySelector("button"))).toBe("section#pricing > div.card > button.buy.primary");
    expect(selectorOf(document.querySelector("span"))).toBe("div > span");
    expect(selectorOf(document.querySelector("em"))).toBe('a[data-track="signup"] > em');
  });

  it("names a form and a field without reading a value", () => {
    document.body.innerHTML = `<form name="newsletter"><input id="mail" value="secret@example.com"></form>`;
    expect(formName(document.querySelector("form"))).toBe("newsletter");
    expect(fieldName(document.querySelector("input"))).toBe("mail");
  });

  it("calls a burst rage only inside the window and the radius", () => {
    expect(isRage([[0, 10, 10], [200, 12, 9], [400, 11, 11]])).toBe(true);
    expect(isRage([[0, 10, 10], [200, 12, 9]])).toBe(false);
    expect(isRage([[0, 10, 10], [700, 12, 9], [1500, 11, 11]])).toBe(false);
    expect(isRage([[0, 10, 10], [100, 200, 9], [200, 11, 400]])).toBe(false);
  });
});

describe("frustration: signals", () => {
  it("reports one rage click per burst, with the clicks counted", async () => {
    document.body.innerHTML = `<div class="hero"><img class="logo" alt=""></div>`;
    const fake = await install("frustration");
    const img = document.querySelector("img")!;
    for (let i = 0; i < 5; i++) click(img);
    const rage = fake.x.mock.calls.filter(([, e]) => e.name === "rage_click");
    expect(rage).toHaveLength(1);
    expect(rage[0][1]).toMatchObject({ raw: true, props: { selector: "div.hero > img.logo", clicks: 3 } });
  });

  it("calls a button that nothing answers dead, once, after a second", async () => {
    vi.useFakeTimers({ toFake: ["setTimeout", "clearTimeout"] });
    document.body.innerHTML = `<button id="save">Save</button><button id="ok">OK</button>`;
    const fake = await install("frustration");
    const save = document.getElementById("save")!;
    click(save);
    click(save, 300, 300);
    vi.advanceTimersByTime(999);
    expect(fake.x).not.toHaveBeenCalled();
    vi.advanceTimersByTime(1);
    const dead = fake.x.mock.calls.filter(([, e]) => e.name === "dead_click");
    expect(dead).toHaveLength(1);
    expect(dead[0][1].props).toEqual({ selector: "button#save" });
  });

  it("does not call a button dead when the page answers it", async () => {
    vi.useFakeTimers({ toFake: ["setTimeout", "clearTimeout"] });
    document.body.innerHTML = `<button id="open">Open</button><div id="panel"></div>`;
    const fake = await install("frustration");
    const open = document.getElementById("open")!;
    open.addEventListener("click", () => {
      document.getElementById("panel")!.textContent = "opened";
    });
    click(open);
    await Promise.resolve(); // the mutation record arrives as a microtask
    vi.advanceTimersByTime(1000);
    expect(fake.x).not.toHaveBeenCalled();
  });

  it("never calls a text field or a plain element dead", async () => {
    vi.useFakeTimers({ toFake: ["setTimeout", "clearTimeout"] });
    document.body.innerHTML = `<input id="q"><p id="text">words</p>`;
    const fake = await install("frustration");
    click(document.getElementById("q")!);
    click(document.getElementById("text")!, 100, 100);
    vi.advanceTimersByTime(2000);
    expect(fake.x).not.toHaveBeenCalled();
  });

  it("reports an abandoned form by its name and its last field, never a value", async () => {
    document.body.innerHTML = `
      <form id="signup"><input name="email"><input name="company"><button>Go</button></form>
      <form id="search"><input name="q"></form>`;
    const fake = await install("frustration");
    const email = document.querySelector<HTMLInputElement>('[name="email"]')!;
    const company = document.querySelector<HTMLInputElement>('[name="company"]')!;
    email.value = "person@example.com";
    email.dispatchEvent(new Event("input", { bubbles: true }));
    company.dispatchEvent(new FocusEvent("focusin", { bubbles: true }));
    // A field only focused, in a form never typed into, is not a form in use.
    document.querySelector('[name="q"]')!.dispatchEvent(new FocusEvent("focusin", { bubbles: true }));

    window.dispatchEvent(new Event("pagehide"));
    const abandoned = fake.x.mock.calls.filter(([, e]) => e.name === "form_abandon");
    expect(abandoned).toHaveLength(1);
    expect(abandoned[0][1].props).toEqual({ form: "signup", field: "company" });
    expect(JSON.stringify(fake.x.mock.calls)).not.toContain("person@example.com");
  });

  it("does not report a form that was submitted, or a query-only navigation", async () => {
    document.body.innerHTML = `<form id="signup"><input name="email"></form>`;
    const fake = await install("frustration");
    const form = document.getElementById("signup")!;
    form.querySelector("input")!.dispatchEvent(new Event("input", { bubbles: true }));
    window.history.pushState({}, "", "/start?step=2");
    expect(fake.x).not.toHaveBeenCalled();
    form.dispatchEvent(new Event("submit", { bubbles: true }));
    window.history.pushState({}, "", "/elsewhere");
    window.dispatchEvent(new Event("pagehide"));
    expect(fake.x).not.toHaveBeenCalled();
  });

  it("reports a form left by a client-side navigation to another path", async () => {
    document.body.innerHTML = `<form id="signup"><input name="email"></form>`;
    const fake = await install("frustration");
    document.querySelector("input")!.dispatchEvent(new Event("change", { bubbles: true }));
    window.history.pushState({}, "", "/pricing");
    expect(fake.x.mock.calls[0][1]).toMatchObject({ name: "form_abandon", props: { form: "signup", field: "email" } });
  });
});

describe("vitals", () => {
  it("reads the observers and reports once, when the core says the page is hidden", async () => {
    const feeds: Record<string, (entries: any[]) => void> = {};
    (window as any).PerformanceObserver = class {
      cb: (l: any) => void;
      constructor(cb: (l: any) => void) {
        this.cb = cb;
      }
      observe(o: { type: string }) {
        feeds[o.type] = (entries) => this.cb({ getEntries: () => entries });
      }
    };
    const fake = await install("vitals");
    feeds["largest-contentful-paint"]([{ startTime: 1180.4 }]);
    feeds["event"]([{ interactionId: 1, duration: 96 }, { interactionId: 0, duration: 400 }]);
    feeds["layout-shift"]([
      { startTime: 100, value: 0.05 },
      { startTime: 300, value: 0.04, hadRecentInput: false },
      { startTime: 400, value: 0.5, hadRecentInput: true },
    ]);
    fake.h!();
    fake.h!();
    expect(fake.x).toHaveBeenCalledTimes(1);
    const [kind, ev] = fake.x.mock.calls[0];
    expect(kind).toBe("web_vitals");
    expect(ev.vitals.lcp).toBe(1180);
    expect(ev.vitals.inp).toBe(96);
    expect(ev.vitals.cls).toBeCloseTo(0.09);
    delete (window as any).PerformanceObserver;
  });

  it("keeps a hook another plugin set before it", async () => {
    const fake = { x: vi.fn(), track: vi.fn(), h: vi.fn() };
    (globalThis as any).__mf = fake;
    const earlier = fake.h;
    vi.resetModules();
    await import("./vitals.js");
    fake.h!();
    expect(earlier).toHaveBeenCalledTimes(1);
  });
});

describe("forms, 404 and click tags", () => {
  it("reports a submission by the form's id or name, and never its fields", async () => {
    document.body.innerHTML = `<form name="signup-form"><input value="a@b.c"></form>`;
    const fake = await install("forms");
    document.querySelector("form")!.dispatchEvent(new Event("submit", { bubbles: true }));
    expect(fake.x).toHaveBeenCalledWith("form", { name: "signup-form" });
    expect(JSON.stringify(fake.x.mock.calls)).not.toContain("a@b.c");
  });

  it("reports a 404 only on a page that carries the marker", async () => {
    let fake = await install("track404");
    expect(fake.x).not.toHaveBeenCalled();
    document.body.setAttribute("data-mf-404", "");
    fake = await install("track404");
    expect(fake.x).toHaveBeenCalledWith("error", { name: "404" });
    document.body.removeAttribute("data-mf-404");
  });

  it("turns a click on a tagged element, or inside one, into a custom event", async () => {
    document.body.innerHTML = `
      <button data-track="signup" data-track-plan="pro" data-track-seats="5"><span>Start</span></button>
      <button>Plain</button>`;
    expect(tagEvent(document.querySelector("span"))).toEqual({
      name: "signup",
      props: { plan: "pro", seats: "5" },
    });
    expect(tagEvent(document.querySelectorAll("button")[1])).toBeNull();

    const fake = await install("tags");
    document.querySelector("span")!.dispatchEvent(new MouseEvent("click", { bubbles: true }));
    document.querySelectorAll("button")[1].dispatchEvent(new MouseEvent("click", { bubbles: true }));
    expect(fake.track).toHaveBeenCalledTimes(1);
    expect(fake.track).toHaveBeenCalledWith("signup", { plan: "pro", seats: "5" });
  });
});
