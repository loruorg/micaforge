import { describe, expect, it } from "vitest";

import { clientIp, header, isConditional, readRequest, requestHost, splitUrl } from "./request.js";

describe("header()", () => {
  it("reads a plain Node bag, case-insensitively", () => {
    expect(header({ "user-agent": "GPTBot" }, "User-Agent")).toBe("GPTBot");
    expect(header({ "User-Agent": "GPTBot" }, "user-agent")).toBe("GPTBot");
  });

  it("reads a Fetch Headers", () => {
    expect(header(new Headers({ "user-agent": "ClaudeBot" }), "user-agent")).toBe("ClaudeBot");
  });

  it("joins a repeated header the way HTTP defines it", () => {
    expect(header({ "x-forwarded-for": ["1.1.1.1", "2.2.2.2"] }, "x-forwarded-for")).toBe("1.1.1.1,2.2.2.2");
  });

  it("returns an empty string rather than throwing on nonsense", () => {
    expect(header(undefined, "x")).toBe("");
  });
});

describe("clientIp()", () => {
  it("takes the first entry of x-forwarded-for, which is the client", () => {
    const req = { headers: { "x-forwarded-for": "203.0.113.9, 10.0.0.1, 10.0.0.2" } };
    expect(clientIp(req)).toBe("203.0.113.9");
  });

  it("prefers a single-value proxy header, which a hop cannot extend", () => {
    const req = {
      headers: { "cf-connecting-ip": "203.0.113.9", "x-forwarded-for": "198.51.100.1" },
    };
    expect(clientIp(req)).toBe("203.0.113.9");
  });

  it("falls back to the socket", () => {
    expect(clientIp({ socket: { remoteAddress: "192.0.2.5" } })).toBe("192.0.2.5");
  });

  it("unwraps an IPv4-mapped IPv6 address, which a dual-stack socket reports", () => {
    expect(clientIp({ socket: { remoteAddress: "::ffff:203.0.113.9" } })).toBe("203.0.113.9");
  });

  it("ignores the proxy headers when trustProxy is off", () => {
    const req = {
      headers: { "x-forwarded-for": "1.2.3.4" },
      socket: { remoteAddress: "192.0.2.5" },
    };
    expect(clientIp(req, false)).toBe("192.0.2.5");
  });
});

describe("splitUrl()", () => {
  it("splits an absolute URL into the four columns that hold it", () => {
    expect(splitUrl("https://docs.example.com/guide?a=1#top")).toEqual({
      hostname: "docs.example.com",
      pathname: "/guide",
      querystring: "a=1",
      hash: "top",
    });
  });

  it("keeps the fallback host for a bare path", () => {
    expect(splitUrl("/guide?a=1", "docs.example.com")).toEqual({
      hostname: "docs.example.com",
      pathname: "/guide",
      querystring: "a=1",
      hash: "",
    });
  });

  it("never throws, whatever it is handed", () => {
    // A broken link in a log line is not a reason to lose the hit. Whatever comes in,
    // a path comes out, and the parser percent-encodes what it cannot leave alone.
    for (const input of ["not a url at all", "/bad%zz", "http://", "///", "%"]) {
      const parts = splitUrl(input, "example.com");
      expect(typeof parts.pathname).toBe("string");
      expect(parts.pathname.length).toBeGreaterThan(0);
    }
  });
});

describe("requestHost()", () => {
  it("prefers x-forwarded-host behind a proxy", () => {
    expect(requestHost({ headers: { host: "internal:3000", "x-forwarded-host": "example.com" } })).toBe(
      "example.com",
    );
  });

  it("strips the port", () => {
    expect(requestHost({ headers: { host: "example.com:8443" } })).toBe("example.com");
  });

  it("keeps an IPv6 literal whole", () => {
    expect(requestHost({ headers: { host: "[2001:db8::1]:443" } })).toBe("2001:db8::1");
  });

  it("reads :authority on HTTP/2", () => {
    expect(requestHost({ headers: { ":authority": "example.com" } })).toBe("example.com");
  });
});

describe("isConditional()", () => {
  it("is true for a re-crawl check", () => {
    expect(isConditional({ headers: { "if-none-match": '"abc"' } })).toBe(true);
    expect(isConditional({ headers: { "if-modified-since": "Thu, 21 Aug 2026 10:00:00 GMT" } })).toBe(true);
  });

  it("is false for a fresh read", () => {
    expect(isConditional({ headers: {} })).toBe(false);
  });
});

describe("readRequest()", () => {
  it("reads everything the SDK needs in one pass", () => {
    const read = readRequest({
      method: "get",
      url: "/docs/x?a=1",
      headers: {
        host: "docs.example.com",
        "user-agent": "GPTBot/1.2",
        referer: "https://chatgpt.com/",
        "accept-language": "en-GB,en;q=0.9",
        "x-forwarded-for": "203.0.113.9",
        "if-none-match": '"v2"',
        accept: "text/html",
        "sec-fetch-mode": "navigate",
        "sec-ch-ua": '"Chromium";v="128"',
      },
    });
    expect(read).toEqual({
      method: "GET",
      url: { hostname: "docs.example.com", pathname: "/docs/x", querystring: "a=1", hash: "" },
      userAgent: "GPTBot/1.2",
      ip: "203.0.113.9",
      referrer: "https://chatgpt.com/",
      language: "en-GB",
      conditional: true,
      headers: {
        accept: "text/html",
        acceptLanguage: "en-GB,en;q=0.9",
        secFetchMode: "navigate",
        secChUa: '"Chromium";v="128"',
      },
    });
  });

  it("reads a Fetch Request, which is what an edge runtime hands you", () => {
    const request = new Request("https://docs.example.com/guide?a=1", {
      headers: { "user-agent": "ClaudeBot/1.0" },
    });
    const read = readRequest(request);
    expect(read.url.hostname).toBe("docs.example.com");
    expect(read.url.pathname).toBe("/guide");
    expect(read.userAgent).toBe("ClaudeBot/1.0");
  });
});
