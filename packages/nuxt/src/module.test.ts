/**
 * The module against a hand-built Nuxt object, through the real `@nuxt/kit`: what it
 * registers and what it writes to the config. No Nuxt app is downloaded or built; the
 * object carries only the fields kit reads.
 */
import { describe, expect, it, vi } from "vitest";
import { runWithNuxtContext } from "@nuxt/kit";
import micaforge, { devtoolsScript, publicConfig, type ModuleOptions } from "./module.js";
import { flagsClient, serverClient } from "./runtime/server/clients.js";

function fakeNuxt(options: ModuleOptions | undefined, dev = true) {
  const hooks: Record<string, ((arg: any) => void)[]> = {};
  const nuxt: any = {
    _version: "4.6.0",
    options: {
      dev,
      micaforge: options,
      plugins: [],
      build: { transpile: [] },
      app: { head: { script: [] } },
      runtimeConfig: { public: {} },
      experimental: {},
      extensions: [".js", ".ts"],
      alias: {},
      _requiredModules: {},
    },
    hooks: { addHooks: () => {} },
    callHook: async () => {},
    hook: (name: string, fn: (arg: any) => void) => {
      (hooks[name] ||= []).push(fn);
      return () => {};
    },
  };
  return { nuxt, hooks };
}

const install = async (options: ModuleOptions | undefined, dev = true) => {
  const { nuxt, hooks } = fakeNuxt(options, dev);
  await runWithNuxtContext(nuxt, () => (micaforge as any)({}, nuxt));
  return { nuxt, hooks };
};

describe("the module", () => {
  it("writes the public and private runtime config and registers its plugins and imports", async () => {
    const { nuxt, hooks } = await install({ siteKey: 1, host: "https://mf.example.com/", plugins: ["errors"], flags: { environment: "staging" } });
    expect(nuxt.options.runtimeConfig.public.micaforge).toMatchObject({
      siteKey: "1",
      host: "https://mf.example.com",
      enabled: true,
      routeTracking: true,
      plugins: { errors: true },
      flags: { environment: "staging" },
    });
    expect(nuxt.options.runtimeConfig.micaforge).toEqual({ ingestKey: "", apiKey: "" });

    const plugins = nuxt.options.plugins.map((p: any) => [p.src.replace(/.*\/runtime\//, "runtime/"), p.mode]);
    expect(plugins).toEqual(
      expect.arrayContaining([
        [expect.stringMatching(/^runtime\/plugin(\.ts)?$/), "all"],
        [expect.stringMatching(/^runtime\/plugin\.client(\.ts)?$/), "client"],
      ]),
    );
    expect(nuxt.options.build.transpile.some((p: string) => p.endsWith("/runtime"))).toBe(true);

    const imports: any[] = [];
    for (const fn of hooks["imports:extend"] || []) fn(imports);
    expect(imports.map((i) => i.name).sort()).toEqual(["useFlag", "useMicaforge"]);
    const nitro: any = {};
    for (const fn of hooks["nitro:config"] || []) fn(nitro);
    expect(nitro.imports.imports.map((i: any) => i.name).sort()).toEqual(["useMicaforgeFlags", "useMicaforgeServer"]);
  });

  it("keeps runtime config set by hand over its own values", async () => {
    const { nuxt } = fakeNuxt({ siteKey: 1, host: "https://mf.example.com" });
    nuxt.options.runtimeConfig.public.micaforge = { siteKey: "7" };
    nuxt.options.runtimeConfig.micaforge = { ingestKey: "from-config" };
    await runWithNuxtContext(nuxt, () => (micaforge as any)({}, nuxt));
    expect(nuxt.options.runtimeConfig.public.micaforge.siteKey).toBe("7");
    expect(nuxt.options.runtimeConfig.micaforge).toEqual({ ingestKey: "from-config", apiKey: "" });
  });

  it("adds no overlay to a production build unless asked", async () => {
    const { nuxt } = await install({ siteKey: 1, host: "https://mf.example.com", devtools: true }, false);
    expect(nuxt.options.app.head.script).toEqual([]);
  });

  it("builds the overlay's tag for development, and for staging when asked", () => {
    expect(devtoolsScript("/*code*/", true, true)).toEqual({ key: "micaforge-devtools", innerHTML: "/*code*/", "data-devtools": "", tagPriority: "critical" });
    expect(devtoolsScript("/*code*/", true, false)).toBeNull();
    expect(devtoolsScript("/*code*/", { production: true, knownEvents: ["signup", "purchase"], open: true }, false)).toMatchObject({
      "data-events": "signup,purchase",
      "data-open": "",
    });
    expect(devtoolsScript(undefined, true, true)).toBeNull();
    expect(devtoolsScript("/*code*/", false, true)).toBeNull();
  });

  it("reads options leniently", () => {
    expect(publicConfig({ siteKey: " 3 ", enabled: false, routeTracking: false, plugins: { vitals: false } })).toMatchObject({
      siteKey: "3",
      host: "",
      enabled: false,
      routeTracking: false,
      plugins: { vitals: false },
    });
  });
});

describe("the server clients", () => {
  it("exist only with the module on and the key set", async () => {
    const fetchMock = vi.fn(async () => new Response("{}", { status: 401 }));
    vi.stubGlobal("fetch", fetchMock);
    const site = { siteKey: "1", host: "https://mf.example.com", enabled: true };
    expect(serverClient(site, {})).toBeUndefined();
    expect(serverClient({ ...site, enabled: false }, { ingestKey: "k" })).toBeUndefined();
    const server = serverClient(site, { ingestKey: "k" });
    expect(server?.siteId).toBe(1);
    expect(server?.endpoint).toBe("https://mf.example.com/api/log/edge");
    await server?.shutdown();

    expect(flagsClient(site, {})).toBeUndefined();
    const flags = flagsClient(site, { apiKey: "mfg_test" });
    expect(flags).toBeDefined();
    await flags?.ready();
    expect(String((fetchMock.mock.calls[0] as unknown[])[0])).toBe("https://mf.example.com/api/flags/ruleset?site=1");
    await flags?.close();
  });
});
