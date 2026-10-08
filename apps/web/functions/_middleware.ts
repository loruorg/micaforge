/**
 * The Pages middleware for micaforge.com: it reports AI agent fetches of the
 * public site to Micaforge itself, and changes nothing about the response.
 *
 * `public/_routes.json` keeps it off the fingerprinted assets, the fonts, the
 * dashboard and the share pages, so it runs only for the pages and files a
 * crawler reads (robots.txt and llms.txt included) and those keep their edge
 * cache. Every failure falls through: the visitor always gets the page.
 */
import { reportAgentFetch } from "../src/lib/edge-report.ts";
import type { MicaforgeEnv } from "../src/lib/edge-report.ts";

/** The slice of the Pages Functions context this file uses. */
interface PagesContext {
  request: Request;
  env: MicaforgeEnv;
  next: () => Promise<Response>;
  waitUntil?: (promise: Promise<unknown>) => void;
}

export async function onRequest(context: PagesContext): Promise<Response> {
  const startedAt = Date.now();
  const response = await context.next();
  context.waitUntil?.(reportAgentFetch(context.env, context.request, response, startedAt));
  return response;
}
