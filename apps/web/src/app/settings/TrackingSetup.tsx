/**
 * TRACKING SETUP: where a new site starts to collect data.
 *
 * The screen has two install panels of equal weight: one for PEOPLE (the
 * browser tag, the npm package, the no-JS pixel) and one for AI CRAWLERS (the
 * server log). They are equal because a crawler runs no JavaScript. Without
 * the second panel half the traffic never arrives and the Citation gap cannot
 * be computed at all. Putting the log shipper third in a list of four would
 * misreport what this product is.
 *
 * Above both sits a status line: waiting while nothing has arrived, and live
 * the moment the first hit lands.
 */
import { useState } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";

import {
  Button,
  CopyField,
  Disclosure,
  Panel,
  PanelHead,
} from "../../components/react/ui";
import { IconCheck } from "../../components/react/icons";
import { publicOrigin, api } from "../../lib/api";
import { ago, num } from "../../lib/format";
import { useSite } from "../providers";
import {
  Caution,
  CheckRow,
  Foot,
  Note,
  QueryState,
  Result,
  Spec,
  SpecRow,
  Stated,
  TextArea,
  Toggle,
  TypedConfirm,
  Waiting,
  useSyncedForm,
} from "./SettingsLayout";
import {
  DEFAULT_TRACKING,
  useSiteRecord,
  type SiteRecord,
  type TrackingOptions,
} from "./SiteSettings";

interface HasData {
  has_data: boolean;
  human_events?: number;
  agent_events?: number;
  first_event_at?: number | string | null;
}

interface IngestKey {
  key: string;
  rotated_at?: string | null;
}

/* ── the install status ──────────────────────────────────────────────────── */

/**
 * A status dot and one line of text: grey while waiting, green once data
 * arrives. Nothing here depends on the soft pulse; the heading, the text and
 * both counts are plain text that render in full with motion disabled.
 */
function Listener({ live, siteId }: { live: HasData | undefined; siteId: string }) {
  const marked = !!live?.has_data;

  return (
    <div className="mf-set-listen" data-live={marked ? "true" : "false"}>
      <span className="mf-set-listen__dot" aria-hidden="true" />

      <div className="mf-set-listen__text">
        <h2 className="mf-set-listen__title">
          {marked ? "This site is live" : "Waiting for the first visit"}
        </h2>
        <p className="mf-set-listen__sub">
          {marked
            ? live?.first_event_at
              ? `First hit recorded ${ago(live.first_event_at)}. Data is arriving.`
              : "Data is arriving."
            : `Nothing has reached site ${siteId} yet. Install one of the options below. This panel checks every few seconds and updates on its own.`}
        </p>

        {typeof live?.human_events === "number" || typeof live?.agent_events === "number" ? (
          <div className="mf-set-listen__chans">
            {typeof live?.human_events === "number" ? (
              <span className="mf-set-listen__chan">
                <span
                  className="mf-drum__swatch"
                  style={{ background: "var(--mf-people)" }}
                  aria-hidden="true"
                />
                People <span className="mf-data">{num(live.human_events)}</span>
              </span>
            ) : null}
            {typeof live?.agent_events === "number" ? (
              <span className="mf-set-listen__chan">
                <span
                  className="mf-drum__swatch"
                  data-pen="stepped"
                  style={{ color: "var(--mf-ai)" }}
                  aria-hidden="true"
                />
                AI crawlers <span className="mf-data">{num(live.agent_events)}</span>
              </span>
            ) : null}
          </div>
        ) : null}
      </div>
    </div>
  );
}

/* ── snippets, built from the options actually set ───────────────────────── */

function scriptTag(siteNo: number, o: TrackingOptions): string {
  const attrs: string[] = [`src="${publicOrigin()}/mf.js"`, `data-site="${siteNo}"`];
  if (o.capture.outbound) attrs.push("data-outbound");
  if (o.capture.downloads) attrs.push("data-downloads");
  if (o.capture.forms) attrs.push("data-forms");
  if (o.capture.not_found) attrs.push("data-404");
  if (o.capture.web_vitals) attrs.push("data-vitals");
  if (o.capture.scroll) attrs.push("data-scroll");
  if (o.hash_routing) attrs.push("data-hash");
  if (o.respect_dnt) attrs.push("data-respect-dnt");
  const paths = o.excluded_paths.map((p) => p.trim()).filter(Boolean);
  if (paths.length) attrs.push(`data-exclude="${paths.join(",")}"`);

  if (attrs.length <= 3) return `<script defer ${attrs.join(" ")}></script>`;
  return `<script defer\n  ${attrs.join("\n  ")}></script>`;
}

function npmInit(siteNo: number, o: TrackingOptions): string {
  const opts: string[] = [`  site: ${siteNo},`, `  host: "${publicOrigin()}",`];
  if (o.capture.outbound) opts.push("  outbound: true,");
  if (o.capture.downloads) opts.push("  downloads: true,");
  if (o.capture.forms) opts.push("  forms: true,");
  if (o.capture.not_found) opts.push("  notFound: true,");
  if (o.capture.web_vitals) opts.push("  vitals: true,");
  if (o.capture.scroll) opts.push("  scroll: true,");
  if (o.hash_routing) opts.push("  hash: true,");
  if (o.respect_dnt) opts.push("  respectDnt: true,");
  const paths = o.excluded_paths.map((p) => p.trim()).filter(Boolean);
  if (paths.length) opts.push(`  exclude: [${paths.map((p) => `"${p}"`).join(", ")}],`);
  return `import { micaforge } from "@micaforge/sdk";\n\nmicaforge.init({\n${opts.join("\n")}\n});`;
}

/* ── the screen ──────────────────────────────────────────────────────────── */

export function TrackingSetup() {
  const { domain } = useSite();
  const { id, query } = useSiteRecord();
  const qc = useQueryClient();
  const [showKey, setShowKey] = useState(false);

  const live = useQuery({
    queryKey: ["settings", "has-data", id],
    queryFn: () => api.get<HasData>(`/api/sites/${id}/has-data`).then((r) => r.data),
    enabled: !!id,
    // Poll until something arrives, then stop asking.
    refetchInterval: (q) => (q.state.data?.has_data ? false : 5000),
  });

  const key = useQuery({
    queryKey: ["settings", "ingest-key", id],
    // The key is part of the site's tracking details; only the key is read here.
    queryFn: () =>
      api
        .get<{ ingest_key: string }>(`/api/sites/${id}/tracking`)
        .then((r): IngestKey => ({ key: r.data.ingest_key })),
    enabled: !!id,
  });

  const rotateKey = useMutation({
    mutationFn: () =>
      api
        .post<{ ingest_key: string; rotated_at: string }>(`/api/sites/${id}/rotate-key`)
        .then((r): IngestKey => ({ key: r.data.ingest_key, rotated_at: r.data.rotated_at })),
    onSuccess: (data) => qc.setQueryData(["settings", "ingest-key", id], data),
  });

  const site = query.data;
  const stored = site?.settings?.tracking;
  const options = useSyncedForm<TrackingOptions>(
    site
      ? {
          ...DEFAULT_TRACKING,
          ...stored,
          capture: { ...DEFAULT_TRACKING.capture, ...stored?.capture },
          excluded_paths: stored?.excluded_paths ?? DEFAULT_TRACKING.excluded_paths,
        }
      : undefined,
  );

  const saveOptions = useMutation({
    mutationFn: (next: TrackingOptions) =>
      api
        .patch<SiteRecord>(`/api/sites/${id}`, {
          settings: { ...(site?.settings ?? {}), tracking: next },
        })
        .then((r) => r.data),
    onSuccess: (data) => qc.setQueryData(["settings", "site", id], data),
  });

  const o = options.form ?? DEFAULT_TRACKING;
  const siteNo = site?.site_id ?? 0;
  const host = domain || "example.com";
  const ingest = key.data?.key;

  return (
    <>
      <Panel>
        <Listener live={live.data} siteId={site ? String(site.site_id) : "this site"} />
        {live.isError ? (
          <Foot tight>
            <Caution>
              Could not ask the server whether anything has arrived. The install below is still correct; this
              panel will confirm once the server answers again.
            </Caution>
          </Foot>
        ) : null}
      </Panel>

      {/* ── tracking people ──────────────────────────────────────────────── */}
      <Panel>
        <PanelHead
          title="Track people"
          note="Three ways to track people in browsers. Pick one. Each snippet below already includes the site number and the capture options set at the bottom of this page."
        />

        <div className="mf-set-intake">
          <div className="mf-set-intake__head">
            <h3 className="mf-set-intake__name">Script tag</h3>
            <span className="mf-set-intake__catch">Under 3 kB. Put it in &lt;head&gt; on every page.</span>
          </div>
          <div className="mf-set-intake__body">
            {query.isPending ? (
              <span className="mf-wait" style={{ width: "70%" }} />
            ) : (
              <CopyField label="HTML" value={scriptTag(siteNo, o)} />
            )}
            <Note>
              It never throws into your page, queues calls made before it loads, and sends with
              <span className="mf-data"> sendBeacon</span> so it cannot delay a navigation.
            </Note>
          </div>
        </div>

        <div className="mf-set-intake">
          <div className="mf-set-intake__head">
            <h3 className="mf-set-intake__name">npm package</h3>
            <span className="mf-set-intake__catch">
              For apps that also send custom events, goals or identify().
            </span>
          </div>
          <div className="mf-set-intake__body">
            <CopyField label="Install" value="npm install @micaforge/sdk" />
            {query.isPending ? (
              <span className="mf-wait" style={{ width: "60%" }} />
            ) : (
              <CopyField label="Initialise once, at the root" value={npmInit(siteNo, o)} />
            )}
          </div>
        </div>

        <div className="mf-set-intake">
          <div className="mf-set-intake__head">
            <h3 className="mf-set-intake__name">Tracking pixel</h3>
            <span className="mf-set-intake__catch">
              No JavaScript at all: email, RSS, an AMP page, a CSP that forbids scripts.
            </span>
          </div>
          <div className="mf-set-intake__body">
            <CopyField
              label="HTML"
              value={`<img src="${publicOrigin()}/api/track/pixel.gif?s=${siteNo}&u=${encodeURIComponent(
                `https://${host}/your-page`,
              )}"\n     width="1" height="1" alt="" />`}
            />
            <Note>
              Replace <span className="mf-data">u</span> with the encoded URL of the page it sits on. The pixel
              records a pageview and nothing else: no engagement time, no scroll, no vitals.
            </Note>
          </div>
        </div>
      </Panel>

      {/* ── tracking AI crawlers ─────────────────────────────────────────── */}
      <Panel>
        <PanelHead
          title="Track AI crawlers"
          note="Crawlers do not run JavaScript, so the options above never see them. AI crawler traffic appears only when your server's access log is sent here, and until then the Citation gap cannot be computed. This is half of the install, not an extra."
        />

        <div className="mf-set-intake">
          <div className="mf-set-intake__head">
            <h3 className="mf-set-intake__name">Log shipper</h3>
            <span className="mf-set-intake__catch">
              Follows an access log, finds each crawler request, and sends nothing else.
            </span>
          </div>
          <div className="mf-set-intake__body">
            {key.isPending ? (
              <span className="mf-wait" style={{ width: "76%" }} />
            ) : key.isError ? (
              <Caution>
                Could not read this site&apos;s ingest key, so the commands below cannot be completed. Reload
                the screen, or check that your role allows reading site keys.
              </Caution>
            ) : (
              <>
                <CopyField
                  label="Docker, following nginx"
                  value={`docker run -d --name micaforge-shipper --restart unless-stopped \\\n  -v /var/log/nginx:/logs:ro -v micaforge-shipper-state:/state \\\n  -e MICAFORGE_HOST=${publicOrigin()} \\\n  -e MICAFORGE_SITE_ID=${siteNo} \\\n  -e MICAFORGE_INGEST_KEY=${ingest ?? "<ingest key>"} \\\n  node:22-alpine npx --yes --package=@micaforge/sdk-server \\\n  micaforge-shipper --state /state/access.micaforge-state /logs/access.log`}
                />
                <CopyField
                  label="Or without Docker"
                  value={`MICAFORGE_HOST=${publicOrigin()} MICAFORGE_SITE_ID=${siteNo} \\\n  MICAFORGE_INGEST_KEY=${ingest ?? "<ingest key>"} \\\n  npx --package=@micaforge/sdk-server micaforge-shipper /var/log/nginx/access.log`}
                />
              </>
            )}
            <Note>
              Combined, common and JSON log formats are detected automatically. The shipper sends only crawler
              requests. It sends the source address so the server can check it against the crawler
              operator&apos;s published address ranges. Micaforge stores a salted, rotating hash of it and
              never the address.
            </Note>
          </div>
        </div>

        <div className="mf-set-intake">
          <div className="mf-set-intake__head">
            <h3 className="mf-set-intake__name">Edge or middleware</h3>
            <span className="mf-set-intake__catch">
              A CDN worker, a reverse-proxy module, or your own framework middleware.
            </span>
          </div>
          <div className="mf-set-intake__body">
            <Note>
              Post each request to <span className="mf-data">POST {publicOrigin()}/api/log/edge</span>, signed with
              the ingest key below. Use this when there is no log file to follow. It records visits from people
              and crawlers alike. The payload and the signature are documented at{" "}
              <span className="mf-data">/docs/server-log</span>.
            </Note>
          </div>
        </div>

        <div className="mf-set-intake">
          <div className="mf-set-intake__head">
            <h3 className="mf-set-intake__name">Ingest key</h3>
            <span className="mf-set-intake__catch">Treat it like a password. It can write events to this site.</span>
          </div>
          <div className="mf-set-intake__body">
            {key.isPending ? (
              <Waiting rows={1} />
            ) : key.isError ? (
              <Note>The key could not be read.</Note>
            ) : showKey ? (
              <CopyField label="Ingest key" value={key.data?.key ?? ""} />
            ) : (
              <div className="mf-set-row">
                <Stated>
                  <span className="mf-data">{(key.data?.key ?? "").slice(0, 10)}</span> and the rest, hidden
                </Stated>
                <Button size="sm" onClick={() => setShowKey(true)}>
                  Show the key
                </Button>
              </div>
            )}
            {key.data?.rotated_at ? (
              <Note>Last rotated {ago(key.data.rotated_at)}.</Note>
            ) : null}

            <Disclosure summary="Rotate the ingest key">
              <TypedConfirm
                phrase={host}
                label="Rotate the key"
                busy={rotateKey.isPending}
                destroys={
                  <>
                    The current key stops working the moment the new one is issued. Every shipper, worker and
                    proxy still using it will be refused until you update them. Nothing already recorded is
                    affected, and the old key cannot be recovered.
                  </>
                }
                onConfirm={() => {
                  setShowKey(true);
                  rotateKey.mutate();
                }}
              />
              <div className="mf-set-row">
                <Result action={rotateKey} done="New key issued. Update your shippers." working="Rotating…" />
              </div>
            </Disclosure>
          </div>
        </div>
      </Panel>

      {/* ── capture options ──────────────────────────────────────────────── */}
      <Panel>
        <PanelHead
          title="What the tracker captures"
          note="These are flags on the tag itself, so an unchecked box is a listener the tracker never binds. Save, then copy the snippet above again."
        />
        <QueryState query={query}>
          {() => (
            <>
              <Spec>
                <SpecRow
                  name="Auto-capture"
                  hint="Each one adds a type of event. Nothing here reads a form field's value."
                >
                  <div className="mf-set-checks mf-set-checks--wide">
                    <CheckRow
                      checked={o.capture.outbound}
                      onChange={(v) => options.set({ capture: { ...o.capture, outbound: v } })}
                      label="Outbound links"
                      hint="A click that leaves the site."
                    />
                    <CheckRow
                      checked={o.capture.downloads}
                      onChange={(v) => options.set({ capture: { ...o.capture, downloads: v } })}
                      label="File downloads"
                      hint="pdf, zip, csv, dmg and the rest."
                    />
                    <CheckRow
                      checked={o.capture.forms}
                      onChange={(v) => options.set({ capture: { ...o.capture, forms: v } })}
                      label="Form submits"
                      hint="The submit event only. No field values are read or sent."
                    />
                    <CheckRow
                      checked={o.capture.not_found}
                      onChange={(v) => options.set({ capture: { ...o.capture, not_found: v } })}
                      label="Not-found pages"
                      hint="Pages your app marks as 404, recorded as their own kind."
                    />
                    <CheckRow
                      checked={o.capture.web_vitals}
                      onChange={(v) => options.set({ capture: { ...o.capture, web_vitals: v } })}
                      label="Web vitals"
                      hint="LCP, CLS, INP, FCP and TTFB, once per page."
                    />
                    <CheckRow
                      checked={o.capture.scroll}
                      onChange={(v) => options.set({ capture: { ...o.capture, scroll: v } })}
                      label="Scroll depth"
                      hint="Deepest scroll reached, sent with the next event."
                    />
                  </div>
                </SpecRow>

                <SpecRow
                  name="Hash routing"
                  hint="For routers that keep the route after a #. Off means an anchor change is not a new pageview."
                >
                  <Toggle
                    label="Treat hash changes as pageviews"
                    checked={o.hash_routing}
                    onChange={(v) => options.set({ hash_routing: v })}
                  />
                </SpecRow>

                <SpecRow
                  name="Respect Do Not Track"
                  hint="When on, a browser sending the DNT header is not recorded at all. Micaforge is cookieless either way, so this is a choice rather than a requirement."
                >
                  <Toggle
                    label="Respect Do Not Track"
                    checked={o.respect_dnt}
                    onChange={(v) => options.set({ respect_dnt: v })}
                  />
                </SpecRow>

                <SpecRow
                  name="Excluded paths"
                  hint="One path per line. * matches within a segment, ** matches across segments. Matches are dropped by the tracker before anything is sent, not hidden afterwards."
                  stack
                >
                  <TextArea
                    data
                    rows={4}
                    spellCheck={false}
                    aria-label="Excluded paths, one per line"
                    placeholder={"/admin/**\n/preview/*\n/internal"}
                    value={o.excluded_paths.join("\n")}
                    onChange={(e) => options.set({ excluded_paths: e.target.value.split("\n") })}
                  />
                </SpecRow>
              </Spec>

              <Foot>
                <Button
                  tone="primary"
                  icon={<IconCheck size={14} />}
                  disabled={!options.dirty || saveOptions.isPending}
                  onClick={() =>
                    saveOptions.mutate({
                      ...o,
                      excluded_paths: o.excluded_paths.map((p) => p.trim()).filter(Boolean),
                    })
                  }
                >
                  Save capture options
                </Button>
                <Button tone="quiet" disabled={!options.dirty} onClick={options.reset}>
                  Discard
                </Button>
                <span className="mf-set-foot__spacer" />
                <Result action={saveOptions} done="Capture options saved." />
              </Foot>
            </>
          )}
        </QueryState>
      </Panel>
    </>
  );
}

export default TrackingSetup;
