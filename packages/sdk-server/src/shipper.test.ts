import { createHmac } from "node:crypto";

import { beforeAll, describe, expect, it } from "vitest";

/**
 * The standalone shipper in `bin/`. It is plain JavaScript with no build step, so it is
 * imported by URL rather than by name; the environment flag stops the import from
 * starting the command-line program.
 */
interface ParsedLine {
  ip: string;
  userAgent: string;
  host: string;
  target: string;
}

interface Shipper {
  parseJsonLine(line: string): ParsedLine | null;
  signBody(ingestKey: string, body: string, atSeconds?: number): string;
}

let shipper: Shipper;

beforeAll(async () => {
  process.env.MICAFORGE_SHIPPER_NO_MAIN = "1";
  const url = new URL("../bin/micaforge-shipper.mjs", import.meta.url).href;
  shipper = (await import(url)) as Shipper;
});

const GPTBOT = "Mozilla/5.0 AppleWebKit/537.36 (KHTML, like Gecko; compatible; GPTBot/1.2)";

function caddyLine(request: Record<string, unknown>): string {
  return JSON.stringify({
    ts: 1787306400.5,
    status: 200,
    size: 512,
    request: {
      method: "GET",
      host: "docs.example.com",
      uri: "/guide",
      headers: { "User-Agent": [GPTBOT] },
      ...request,
    },
  });
}

describe("the shipper's log parser", () => {
  it("reads client_ip before remote_ip, so a proxied crawler is verified by its own address", () => {
    const parsed = shipper.parseJsonLine(
      caddyLine({ remote_ip: "173.245.48.1", client_ip: "20.171.207.130" }),
    );
    expect(parsed?.ip).toBe("20.171.207.130");
    expect(parsed?.userAgent).toBe(GPTBOT);
    expect(parsed?.host).toBe("docs.example.com");
  });

  it("falls back to remote_ip when there is no client_ip", () => {
    const parsed = shipper.parseJsonLine(caddyLine({ remote_ip: "20.171.207.130" }));
    expect(parsed?.ip).toBe("20.171.207.130");
  });

  it("prefers client_ip in a flat JSON log too", () => {
    const parsed = shipper.parseJsonLine(
      JSON.stringify({
        remote_addr: "10.0.0.2",
        client_ip: "20.171.207.130",
        http_user_agent: GPTBOT,
        request_uri: "/guide",
        host: "docs.example.com",
      }),
    );
    expect(parsed?.ip).toBe("20.171.207.130");
  });

  it("does not throw on a line that is not JSON", () => {
    expect(shipper.parseJsonLine("not json")).toBeNull();
  });
});

describe("the shipper's signature", () => {
  it("is the same `${t}.${body}` HMAC the SDK and the server use", () => {
    const body = '{"site_id":1}';
    const expected = createHmac("sha256", "mfi_key").update(`1787306400.${body}`).digest("hex");
    expect(shipper.signBody("mfi_key", body, 1_787_306_400)).toBe(`t=1787306400,v1=${expected}`);
  });
});
