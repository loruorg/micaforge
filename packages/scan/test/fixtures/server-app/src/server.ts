import { Micaforge } from "@micaforge/sdk-server";

export const micaforge = new Micaforge({
  host: process.env.MICAFORGE_HOST ?? "",
  siteId: 1,
  ingestKey: process.env.MICAFORGE_INGEST_KEY ?? "",
});

export function checkout(req: Request, url: string) {
  micaforge.track({ name: "purchase", url, props: { plan: "pro", revenue: "49", currency: "euro" }, revenue: 49 });
  micaforge.pageview(req, { name: "checkout_view", props: { step: 1 } });
  micaforge.batch([
    { type: "event", name: "import_done", props: { rows: 10 } },
    { type: "agent", userAgent: "GPTBot/1.2", url },
  ]);
  // A pageview, not a custom event.
  micaforge.track({ url: "https://example.com/" });
}
