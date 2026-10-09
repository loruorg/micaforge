/**
 * THE STATUS PAGE EDITOR: make a public status page, or change one.
 *
 * A status page is read by the site's own customers, so most of this form is
 * about what THEY will see: the address, the title and words, the brand
 * accent, a link back home, and which monitors appear, in which order and
 * under which name. The monitor's own name is often an internal one ("api-2
 * health"), so each monitor on the page can carry a public display name, and
 * the monitored URL itself is never published.
 *
 * The order is set with Up and Down buttons rather than by dragging. A button
 * is reachable by keyboard, says what it does, and cannot drop a row in the
 * wrong place by a pixel.
 *
 * The accent colour is the one colour in the dashboard that is data rather
 * than a token: the page owner chooses it. It is offered as a native colour
 * picker beside the hex text, and both edit the same value. The public page
 * uses it only for its top rule and its links; a state there is always said in
 * words and shapes, so a red brand does not make every monitor look down.
 *
 * Changing a page needs the admin role. The form does not guess the reader's
 * role: it sends the change and prints the server's answer, which names the
 * role when the role is the problem.
 */
import { useId, useMemo, useState } from "react";
import { Link, useParams } from "react-router";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Button, Panel, PanelHead } from "../../components/react/ui";
import { IconCheck, IconChevron, IconClose, IconPlus } from "../../components/react/icons";
import { api } from "../../lib/api";
import { Check, Field, Input, Say, Select, TextArea } from "./parts";
import type { Monitor, StatusPage, StatusPageInput } from "./types";
import { fieldOf, isHttpUrl, keys, problem } from "./types";

/** The API's default accent. Data, not a design token: the owner may change it. */
const DEFAULT_ACCENT = "#2f6bff";
const HEX = /^#[0-9a-fA-F]{6}$/;
const SLUG = /^[a-z0-9-]{3,64}$/;

interface PageDraft {
  slug: string;
  title: string;
  description: string;
  logoUrl: string;
  accent: string;
  footerLabel: string;
  footerUrl: string;
  published: boolean;
  monitors: { monitor_id: string; display_name: string }[];
}

function fromPage(p: StatusPage): PageDraft {
  return {
    slug: p.slug,
    title: p.title,
    description: p.description,
    logoUrl: p.logo_url ?? "",
    accent: p.accent_color,
    footerLabel: p.footer_label ?? "",
    footerUrl: p.footer_url ?? "",
    published: p.published,
    monitors: [...p.monitors]
      .sort((a, b) => a.position - b.position)
      .map((m) => ({ monitor_id: m.monitor_id, display_name: m.display_name ?? "" })),
  };
}

/** A first slug from the site's domain: "docs.example.com" becomes "docs-example-com". */
function slugFrom(domain: string | undefined): string {
  const s = (domain ?? "")
    .toLowerCase()
    .replace(/^www\./, "")
    .replace(/[^a-z0-9]+/g, "-")
    .replace(/^-+|-+$/g, "")
    .slice(0, 64);
  return s.length >= 3 ? s : "";
}

function blank(domain: string | undefined): PageDraft {
  return {
    slug: slugFrom(domain),
    title: domain ? `${domain} status` : "",
    description: "",
    logoUrl: "",
    accent: DEFAULT_ACCENT,
    footerLabel: "",
    footerUrl: "",
    published: false,
    monitors: [],
  };
}

function toInput(d: PageDraft): StatusPageInput {
  return {
    slug: d.slug.trim(),
    title: d.title.trim(),
    description: d.description.trim(),
    logo_url: d.logoUrl.trim() || null,
    accent_color: d.accent.toLowerCase(),
    footer_label: d.footerLabel.trim() || null,
    footer_url: d.footerUrl.trim() || null,
    published: d.published,
    monitors: d.monitors.map((m) => ({ monitor_id: m.monitor_id, display_name: m.display_name.trim() || null })),
  };
}

function check(d: PageDraft): Record<string, string> {
  const out: Record<string, string> = {};
  if (!SLUG.test(d.slug.trim())) out.slug = "Use 3 to 64 characters: lower-case letters, numbers and dashes.";
  const t = d.title.trim();
  if (!t) out.title = "Give the page a title. Readers see it at the top of the page.";
  else if (t.length > 120) out.title = "Keep the title to 120 characters or fewer.";
  if (d.description.trim().length > 1000) out.description = "Keep the description to 1,000 characters or fewer.";
  if (d.logoUrl.trim() && !isHttpUrl(d.logoUrl.trim(), true)) out.logo_url = "Enter a full https:// address of an image.";
  if (!HEX.test(d.accent)) out.accent_color = "Enter a colour as # and six hex digits, for example #2f6bff.";
  const label = d.footerLabel.trim();
  const url = d.footerUrl.trim();
  if (label && !url) out.footer_url = "Enter the address the link goes to, or clear the label.";
  if (url && !label) out.footer_label = "Enter the words of the link, or clear the address.";
  if (url && !isHttpUrl(url)) out.footer_url = "Enter a full address that starts with http:// or https://.";
  return out;
}

const FIELDS = new Set([
  "slug",
  "title",
  "description",
  "logo_url",
  "accent_color",
  "footer_label",
  "footer_url",
  "published",
  "monitors",
]);

/** "monitors[2].display_name" and "monitors.2" both belong to the monitors group. */
const fieldGroup = (f: string | undefined): string | undefined =>
  f == null ? undefined : FIELDS.has(f) ? f : f.startsWith("monitors") ? "monitors" : undefined;

export function StatusPageEditor({
  siteId,
  domain,
  page,
  onDone,
}: {
  siteId: number;
  domain: string | undefined;
  page?: StatusPage;
  onDone: (saved?: StatusPage) => void;
}) {
  const uid = useId();
  const qc = useQueryClient();
  const { site } = useParams();
  const [draft, setDraft] = useState<PageDraft>(() => (page ? fromPage(page) : blank(domain)));
  const [tried, setTried] = useState(false);
  const [server, setServer] = useState<{ field?: string; message: string } | null>(null);
  const [adding, setAdding] = useState("");

  const monitors = useQuery({
    queryKey: keys.monitors(siteId),
    queryFn: () => api.get<Monitor[]>("/api/uptime/monitors", { site: siteId }).then((r) => r.data),
  });

  /** Names for every monitor the page may show, including one the list has not loaded. */
  const names = useMemo(() => {
    const m = new Map<string, Monitor | { name: string; url?: undefined }>();
    for (const pm of page?.monitors ?? []) m.set(pm.monitor_id, { name: pm.name });
    for (const mon of monitors.data ?? []) m.set(mon.id, mon);
    return m;
  }, [page, monitors.data]);

  const set = (patch: Partial<PageDraft>) => {
    setDraft((d) => ({ ...d, ...patch }));
    setServer(null);
  };

  const local = tried ? check(draft) : {};
  const errFor = (k: string): string | undefined => local[k] ?? (server?.field === k ? server.message : undefined);
  const bind = (k: string) => ({
    id: `${uid}-${k}`,
    "aria-invalid": errFor(k) ? true : undefined,
    "aria-describedby": errFor(k) ? `${uid}-${k}-err` : undefined,
  });

  const save = useMutation({
    mutationFn: (input: StatusPageInput) =>
      page
        ? api.patch<StatusPage>(`/api/status-pages/${page.id}`, input).then((r) => r.data)
        : api.post<StatusPage>(`/api/status-pages?site=${siteId}`, input).then((r) => r.data),
    onSuccess: async (saved) => {
      await qc.invalidateQueries({ queryKey: keys.pages(siteId) });
      onDone(saved);
    },
    onError: (e) => setServer({ field: fieldGroup(fieldOf(e)), message: problem(e) }),
  });

  const submit = () => {
    setTried(true);
    setServer(null);
    if (Object.keys(check(draft)).length) return;
    save.mutate(toInput(draft));
  };

  const move = (i: number, by: -1 | 1) => {
    const list = [...draft.monitors];
    const j = i + by;
    if (j < 0 || j >= list.length) return;
    [list[i], list[j]] = [list[j], list[i]];
    set({ monitors: list });
  };

  const chosen = new Set(draft.monitors.map((m) => m.monitor_id));
  const remaining = (monitors.data ?? []).filter((m) => !chosen.has(m.id));
  const origin = typeof window !== "undefined" ? window.location.origin : "";
  const unplaced = server && !server.field ? server.message : null;
  const accentOk = HEX.test(draft.accent);

  return (
    <Panel className="mf-mon-editor">
      <PanelHead
        title={page ? `Edit ${page.title}` : "New status page"}
        note="Readers need no account. Only the monitors you add below appear on the page, and their addresses are never shown."
      />
      <form
        className="mf-mon-form"
        noValidate
        onSubmit={(e) => {
          e.preventDefault();
          submit();
        }}
      >
        <fieldset className="mf-mon-form__part">
          <legend className="mf-mon-form__title">The page</legend>
          <div className="mf-mon-grid">
            <Field
              id={`${uid}-slug`}
              label="Address"
              hint={`${origin}/status/${draft.slug.trim() || "…"}`}
              error={errFor("slug")}
            >
              <Input
                {...bind("slug")}
                data
                spellCheck={false}
                autoComplete="off"
                autoFocus={!page}
                maxLength={64}
                value={draft.slug}
                onChange={(e) => set({ slug: e.target.value.toLowerCase() })}
              />
            </Field>
            <Field id={`${uid}-title`} label="Title" error={errFor("title")}>
              <Input {...bind("title")} maxLength={120} value={draft.title} onChange={(e) => set({ title: e.target.value })} />
            </Field>
            <Field
              id={`${uid}-description`}
              label="Description"
              hint="Optional. One or two sentences under the title."
              error={errFor("description")}
              wide
            >
              <TextArea
                {...bind("description")}
                rows={2}
                maxLength={1000}
                value={draft.description}
                onChange={(e) => set({ description: e.target.value })}
              />
            </Field>
            <Field id={`${uid}-logo_url`} label="Logo address" hint="Optional. An https:// image." error={errFor("logo_url")}>
              <Input
                {...bind("logo_url")}
                data
                type="url"
                spellCheck={false}
                placeholder="https://example.com/logo.svg"
                value={draft.logoUrl}
                onChange={(e) => set({ logoUrl: e.target.value })}
              />
            </Field>
            <Field
              id={`${uid}-accent_color`}
              label="Accent colour"
              hint="The line at the top of the page and its links. States keep their own colours and shapes."
              error={errFor("accent_color")}
            >
              <span className="mf-mon-colour">
                <input
                  type="color"
                  className="mf-mon-colour__pick"
                  aria-label="Choose the accent colour"
                  value={accentOk ? draft.accent.toLowerCase() : DEFAULT_ACCENT}
                  onChange={(e) => set({ accent: e.target.value })}
                />
                <Input
                  {...bind("accent_color")}
                  data
                  spellCheck={false}
                  maxLength={7}
                  value={draft.accent}
                  onChange={(e) => set({ accent: e.target.value.trim() })}
                />
              </span>
            </Field>
            <Field id={`${uid}-footer_label`} label="Footer link words" hint="Optional, for example “Back to example.com”." error={errFor("footer_label")}>
              <Input
                {...bind("footer_label")}
                maxLength={120}
                value={draft.footerLabel}
                onChange={(e) => set({ footerLabel: e.target.value })}
              />
            </Field>
            <Field id={`${uid}-footer_url`} label="Footer link address" error={errFor("footer_url")}>
              <Input
                {...bind("footer_url")}
                data
                type="url"
                spellCheck={false}
                placeholder="https://example.com"
                value={draft.footerUrl}
                onChange={(e) => set({ footerUrl: e.target.value })}
              />
            </Field>
          </div>
          <Check
            id={`${uid}-published`}
            checked={draft.published}
            onChange={(v) => set({ published: v })}
            label="Published"
            hint="Until it is published, the address says there is no status page there."
          />
          {errFor("published") ? <p className="mf-mon-field__err">{errFor("published")}</p> : null}
        </fieldset>

        <fieldset className="mf-mon-form__part" aria-describedby={errFor("monitors") ? `${uid}-monitors-err` : undefined}>
          <legend className="mf-mon-form__title">Monitors on the page</legend>
          {draft.monitors.length ? (
            <ol className="mf-mon-pick">
              {draft.monitors.map((m, i) => {
                const known = names.get(m.monitor_id);
                const name = known?.name ?? "A deleted monitor";
                return (
                  <li key={m.monitor_id} className="mf-mon-pick__row">
                    <span className="mf-mon-pick__pos mf-data" aria-hidden="true">
                      {i + 1}
                    </span>
                    <span className="mf-mon-pick__who">
                      <span className="mf-mon-pick__name">{name}</span>
                      {known?.url ? <span className="mf-data mf-mon-pick__url">{known.url}</span> : null}
                    </span>
                    <span className="mf-mon-pick__as">
                      <Input
                        aria-label={`Name shown on the page for ${name}`}
                        placeholder={`Shown as "${name}"`}
                        maxLength={120}
                        value={m.display_name}
                        onChange={(e) =>
                          set({
                            monitors: draft.monitors.map((x, k) => (k === i ? { ...x, display_name: e.target.value } : x)),
                          })
                        }
                      />
                    </span>
                    <span className="mf-mon-pick__acts">
                      <Button
                        size="sm"
                        tone="quiet"
                        className="mf-mon-up"
                        icon={<IconChevron size={13} />}
                        aria-label={`Move ${name} up`}
                        disabled={i === 0}
                        onClick={() => move(i, -1)}
                      />
                      <Button
                        size="sm"
                        tone="quiet"
                        icon={<IconChevron size={13} />}
                        aria-label={`Move ${name} down`}
                        disabled={i === draft.monitors.length - 1}
                        onClick={() => move(i, 1)}
                      />
                      <Button
                        size="sm"
                        tone="quiet"
                        icon={<IconClose size={13} />}
                        aria-label={`Remove ${name} from the page`}
                        onClick={() => set({ monitors: draft.monitors.filter((_, k) => k !== i) })}
                      />
                    </span>
                  </li>
                );
              })}
            </ol>
          ) : (
            <p className="mf-mon-quiet">No monitors on this page yet. The page shows its title, incidents and maintenance, and no monitor rows.</p>
          )}

          {monitors.isError ? (
            <Say tone="warn">The site's monitors did not load: {problem(monitors.error)}</Say>
          ) : monitors.isSuccess && monitors.data.length === 0 ? (
            <p className="mf-mon-quiet">
              This site has no monitors yet. <Link to={`/app/${site ?? ""}/uptime`}>Add one on the Uptime screen</Link>, then
              choose it here.
            </p>
          ) : remaining.length ? (
            <div className="mf-mon-row">
              <Select aria-label="Monitor to add" value={adding} onChange={(e) => setAdding(e.target.value)}>
                <option value="">Choose a monitor…</option>
                {remaining.map((m) => (
                  <option key={m.id} value={m.id}>
                    {m.name}
                  </option>
                ))}
              </Select>
              <Button
                size="sm"
                icon={<IconPlus size={13} />}
                disabled={!adding}
                onClick={() => {
                  set({ monitors: [...draft.monitors, { monitor_id: adding, display_name: "" }] });
                  setAdding("");
                }}
              >
                Add to page
              </Button>
            </div>
          ) : null}
          {errFor("monitors") ? (
            <p className="mf-mon-field__err" id={`${uid}-monitors-err`}>
              {errFor("monitors")}
            </p>
          ) : null}
        </fieldset>

        <div className="mf-mon-acts">
          <Button type="submit" tone="primary" icon={<IconCheck size={14} />} disabled={save.isPending}>
            {save.isPending ? "Saving…" : page ? "Save changes" : "Create page"}
          </Button>
          <Button tone="quiet" disabled={save.isPending} onClick={() => onDone()}>
            Cancel
          </Button>
          {tried && Object.keys(local).length ? <Say tone="warn">Fix the fields marked above, then try again.</Say> : null}
          {unplaced ? <Say tone="warn">{unplaced}</Say> : null}
        </div>
      </form>
    </Panel>
  );
}

export default StatusPageEditor;
