/**
 * The short-link editor: one form for a new link and for an existing one.
 *
 * The form is in the order an operator thinks about a link, not the order of
 * the database row:
 *
 *   1. ADDRESS. Where it goes, what it is called, which folder it lives in.
 *   2. CAMPAIGN TAGS. The five utm_* tags, with the final address written out
 *      live underneath, because a tag typed into a box is abstract and the URL
 *      it produces is not. The live address is built by `finalUrl`, which
 *      follows the server's rule; after saving, the server's own `final_url` is
 *      what the link panel shows.
 *   3. PREVIEW CARD. Title, description and image for chat apps, with a card
 *      that redraws as they are typed.
 *   4. PHONES AND APPS. Store links and an app's own scheme. A phone tries the
 *      app first and falls back to the web target, and the form says so in one
 *      line rather than leaving the order to be guessed.
 *   5. EXPIRY. A local date and time, sent as UTC.
 *
 * Validation is mostly the server's. The form checks only what it can check
 * for certain (a target is required and must be http(s); a slug must fit its
 * pattern), and every other error the server returns is written under the
 * field `error.field` names. A 409 is the one exception with its own words: it
 * always means the slug is taken.
 */
import type { SubmitEvent } from "react";
import { useEffect, useRef, useState } from "react";
import { useMutation, useQueryClient } from "@tanstack/react-query";

import { Button, Panel, PanelHead } from "../../components/react/ui";
import { IconCheck, IconClose } from "../../components/react/icons";
import { api, ApiError } from "../../lib/api";
import { hostOf } from "../../lib/format";
import { Choice, Field, Say, TextArea, TextInput } from "./controls";
import { PreviewCard } from "./PreviewCard";
import {
  SLUG_RE,
  UTM_KEYS,
  cleanUtm,
  finalUrl,
  fromLocalInput,
  isWebUrl,
  localZone,
  problem,
  scopeQuery,
  toLocalInput,
} from "./model";
import type { Folder, Link, LinkInput, LinkScope, LinksConfig, UtmKey } from "./model";

type FieldKey =
  | "slug"
  | "target_url"
  | "utm"
  | "folder_id"
  | "title"
  | "description"
  | "image_url"
  | "ios_url"
  | "android_url"
  | "deep_link"
  | "expires_at";

const FIELD_KEYS: readonly FieldKey[] = [
  "slug",
  "target_url",
  "utm",
  "folder_id",
  "title",
  "description",
  "image_url",
  "ios_url",
  "android_url",
  "deep_link",
  "expires_at",
];

interface Draft {
  slug: string;
  target: string;
  utm: Record<UtmKey, string>;
  folder: string;
  title: string;
  description: string;
  image: string;
  ios: string;
  android: string;
  deep: string;
  expires: string;
}

const UTM_FIELDS: Record<UtmKey, { label: string; example: string }> = {
  source: { label: "Source", example: "newsletter" },
  medium: { label: "Medium", example: "email" },
  campaign: { label: "Campaign", example: "october-launch" },
  term: { label: "Term", example: "analytics" },
  content: { label: "Content", example: "header-button" },
};

function draftOf(link: Link | null): Draft {
  return {
    slug: link?.slug ?? "",
    target: link?.target_url ?? "",
    utm: {
      source: link?.utm?.source ?? "",
      medium: link?.utm?.medium ?? "",
      campaign: link?.utm?.campaign ?? "",
      term: link?.utm?.term ?? "",
      content: link?.utm?.content ?? "",
    },
    folder: link?.folder_id ?? "",
    title: link?.title ?? "",
    description: link?.description ?? "",
    image: link?.image_url ?? "",
    ios: link?.ios_url ?? "",
    android: link?.android_url ?? "",
    deep: link?.deep_link ?? "",
    expires: toLocalInput(link?.expires_at ?? null),
  };
}

const orNull = (v: string): string | null => (v.trim() ? v.trim() : null);

function bodyOf(d: Draft, link: Link | null): LinkInput {
  const body: LinkInput = {
    folder_id: d.folder || null,
    target_url: d.target.trim(),
    utm: cleanUtm(d.utm),
    title: orNull(d.title),
    description: orNull(d.description),
    image_url: orNull(d.image),
    ios_url: orNull(d.ios),
    android_url: orNull(d.android),
    deep_link: orNull(d.deep),
    expires_at: fromLocalInput(d.expires),
  };
  const slug = d.slug.trim();
  // An emptied slug on an existing link keeps the one it has: a generated
  // replacement would silently break every copy of the old address.
  if (slug) body.slug = slug;
  else if (link) body.slug = link.slug;
  return body;
}

/** What the form can say for certain before the server is asked. */
function validate(d: Draft): Partial<Record<FieldKey, string>> {
  const out: Partial<Record<FieldKey, string>> = {};
  if (!d.target.trim()) out.target_url = "Enter the address this link sends people to.";
  else if (!isWebUrl(d.target)) out.target_url = "Enter a full address that starts with https:// or http://.";
  const slug = d.slug.trim();
  if (slug && !SLUG_RE.test(slug)) out.slug = "Use 1 to 64 letters, digits, hyphens or underscores. No spaces.";
  if (d.expires && !fromLocalInput(d.expires)) out.expires_at = "Enter a valid date and time, or clear the field.";
  return out;
}

/** The form field a server error names. */
function fieldOf(e: unknown): FieldKey | null {
  if (!(e instanceof ApiError)) return null;
  if (e.status === 409) return "slug";
  const f = (e.field ?? "").trim();
  if (!f) return null;
  if (f.startsWith("utm")) return "utm";
  return (FIELD_KEYS as readonly string[]).includes(f) ? (f as FieldKey) : null;
}

function serverWords(e: unknown): string {
  if (e instanceof ApiError && e.status === 409)
    return "This slug is already taken. Choose another, or leave the field empty and the server picks one.";
  return problem(e);
}

export function LinkEditor({
  link,
  scope,
  folders,
  config,
  onSaved,
  onCancel,
}: {
  /** The link being edited, or null for a new one. */
  link: Link | null;
  scope: LinkScope;
  folders: Folder[];
  config: LinksConfig | undefined;
  onSaved: (link: Link) => void;
  onCancel: () => void;
}) {
  const qc = useQueryClient();
  const [d, setD] = useState<Draft>(() => draftOf(link));
  const [checked, setChecked] = useState<Partial<Record<FieldKey, string>>>({});
  const targetRef = useRef<HTMLInputElement>(null);
  const slugRef = useRef<HTMLInputElement>(null);

  useEffect(() => {
    targetRef.current?.focus();
  }, []);

  const save = useMutation({
    mutationFn: (body: LinkInput) =>
      (link
        ? api.patch<Link>(`/api/links/${encodeURIComponent(link.id)}`, body)
        : api.post<Link>(`/api/links?${scopeQuery(scope)}`, body)
      ).then((r) => r.data),
    onSuccess: (saved) => {
      void qc.invalidateQueries({ queryKey: ["links"] });
      onSaved(saved);
    },
  });

  const set = (patch: Partial<Draft>) => {
    const next = { ...d, ...patch };
    setD(next);
    // Once a save has been refused, the marks follow the typing, so a fixed
    // field stops saying it is wrong without another press of Save.
    if (Object.keys(checked).length) setChecked(validate(next));
    if (save.isError) save.reset();
  };
  const setUtm = (k: UtmKey, v: string) => set({ utm: { ...d.utm, [k]: v } });

  const serverField = save.isError ? fieldOf(save.error) : null;
  const errorFor = (k: FieldKey): string | null =>
    checked[k] ?? (serverField === k ? serverWords(save.error) : null);
  const formError = save.isError && serverField == null ? serverWords(save.error) : null;

  const submit = (e: SubmitEvent) => {
    e.preventDefault();
    const found = validate(d);
    setChecked(found);
    if (found.target_url) {
      targetRef.current?.focus();
      return;
    }
    if (Object.keys(found).length) {
      if (found.slug) slugRef.current?.focus();
      return;
    }
    save.mutate(bodyOf(d, link));
  };

  const shortBase = (config?.short_base ?? "").replace(/\/+$/, "");
  const shortHost = shortBase ? hostOf(shortBase) : "your short-link host";
  const live = d.target.trim() ? finalUrl(d.target, d.utm) : "";
  const hasTags = Object.keys(cleanUtm(d.utm)).length > 0;
  const expiresPast = (() => {
    const iso = fromLocalInput(d.expires);
    return iso != null && Date.parse(iso) <= Date.now();
  })();

  return (
    <Panel className="mf-gr-editor">
      <PanelHead
        title={link ? `Edit /${link.slug}` : "New short link"}
        note={
          link
            ? "Changes apply to the next click. The short address stays the same unless you change the slug."
            : "Only the target is required. Everything else is optional."
        }
        actions={
          <Button tone="quiet" size="sm" icon={<IconClose size={13} />} onClick={onCancel}>
            Close
          </Button>
        }
      />

      <form className="mf-gr-form" onSubmit={submit} noValidate>
        {/* ── address ─────────────────────────────────────────────── */}
        <fieldset className="mf-gr-set">
          <legend className="mf-gr-set__title">Address</legend>
          <div className="mf-gr-grid">
            <Field
              wide
              label="Target URL"
              hint="The page the short link sends people to."
              error={errorFor("target_url")}
            >
              {(id, desc) => (
                <TextInput
                  ref={targetRef}
                  id={id}
                  mono
                  type="url"
                  inputMode="url"
                  autoComplete="off"
                  spellCheck={false}
                  placeholder="https://example.com/pricing"
                  value={d.target}
                  invalid={!!errorFor("target_url")}
                  aria-describedby={desc}
                  onChange={(e) => set({ target: e.target.value })}
                />
              )}
            </Field>
            <Field
              label="Slug"
              hint={
                link
                  ? `Leave empty to keep /${link.slug}.`
                  : "Leave empty and the server picks a 7-character slug."
              }
              error={errorFor("slug")}
            >
              {(id, desc) => (
                <span className="mf-gr-affix">
                  <span className="mf-gr-affix__pre mf-data" aria-hidden="true">
                    {shortHost}/
                  </span>
                  <TextInput
                    ref={slugRef}
                    id={id}
                    mono
                    autoComplete="off"
                    spellCheck={false}
                    maxLength={64}
                    placeholder={link ? link.slug : "launch"}
                    value={d.slug}
                    invalid={!!errorFor("slug")}
                    aria-describedby={desc}
                    onChange={(e) => set({ slug: e.target.value })}
                  />
                </span>
              )}
            </Field>
            <Field label="Folder" error={errorFor("folder_id")}>
              {(id, desc) => (
                <Choice
                  id={id}
                  value={d.folder}
                  aria-describedby={desc}
                  onChange={(e) => set({ folder: e.target.value })}
                >
                  <option value="">No folder</option>
                  {folders.map((f) => (
                    <option key={f.id} value={f.id}>
                      {f.name}
                    </option>
                  ))}
                </Choice>
              )}
            </Field>
          </div>
        </fieldset>

        {/* ── campaign tags ───────────────────────────────────────── */}
        <fieldset className="mf-gr-set">
          <legend className="mf-gr-set__title">Campaign tags</legend>
          <p className="mf-gr-set__note">
            UTM tags are added to the target, so the page you send people to can report where they came
            from. Empty tags are left out.
          </p>
          <div className="mf-gr-grid mf-gr-grid--utm">
            {UTM_KEYS.map((k) => (
              <Field key={k} label={UTM_FIELDS[k].label}>
                {(id) => (
                  <TextInput
                    id={id}
                    autoComplete="off"
                    spellCheck={false}
                    placeholder={UTM_FIELDS[k].example}
                    value={d.utm[k]}
                    invalid={serverField === "utm"}
                    onChange={(e) => setUtm(k, e.target.value)}
                  />
                )}
              </Field>
            ))}
          </div>
          {errorFor("utm") ? <Say tone="warn">{errorFor("utm")}</Say> : null}
          <div className="mf-gr-final">
            <span className="mf-gr-final__label">{hasTags ? "Final URL" : "Final URL, no tags added"}</span>
            {live ? (
              <code className="mf-gr-final__url" aria-live="polite">
                {live}
              </code>
            ) : (
              <span className="mf-gr-final__none">Enter a target URL to see the final address.</span>
            )}
          </div>
        </fieldset>

        {/* ── preview card ───────────────────────────────────────── */}
        <fieldset className="mf-gr-set">
          <legend className="mf-gr-set__title">Preview card</legend>
          <p className="mf-gr-set__note">
            What a chat app or social site shows when someone pastes the short link.
          </p>
          <div className="mf-gr-split">
            <div className="mf-gr-grid mf-gr-grid--one">
              <Field label="Title" error={errorFor("title")}>
                {(id, desc) => (
                  <TextInput
                    id={id}
                    maxLength={300}
                    value={d.title}
                    invalid={!!errorFor("title")}
                    aria-describedby={desc}
                    onChange={(e) => set({ title: e.target.value })}
                  />
                )}
              </Field>
              <Field label="Description" error={errorFor("description")}>
                {(id, desc) => (
                  <TextArea
                    id={id}
                    rows={3}
                    maxLength={1000}
                    value={d.description}
                    invalid={!!errorFor("description")}
                    aria-describedby={desc}
                    onChange={(e) => set({ description: e.target.value })}
                  />
                )}
              </Field>
              <Field
                label="Image URL"
                hint="A 1200 by 630 image works in most apps."
                error={errorFor("image_url")}
              >
                {(id, desc) => (
                  <TextInput
                    id={id}
                    mono
                    type="url"
                    inputMode="url"
                    autoComplete="off"
                    spellCheck={false}
                    placeholder="https://example.com/card.png"
                    value={d.image}
                    invalid={!!errorFor("image_url")}
                    aria-describedby={desc}
                    onChange={(e) => set({ image: e.target.value })}
                  />
                )}
              </Field>
            </div>
            <PreviewCard title={d.title} description={d.description} imageUrl={d.image} host={shortHost} />
          </div>
        </fieldset>

        {/* ── phones and apps ───────────────────────────────────── */}
        <fieldset className="mf-gr-set">
          <legend className="mf-gr-set__title">Phones and apps</legend>
          <p className="mf-gr-set__note">
            Device addresses send iPhone and Android visitors somewhere other than the target, such as an
            app store.
          </p>
          <div className="mf-gr-grid">
            <Field label="iOS URL" hint="For iPhone and iPad. An App Store link or a web address." error={errorFor("ios_url")}>
              {(id, desc) => (
                <TextInput
                  id={id}
                  mono
                  inputMode="url"
                  autoComplete="off"
                  spellCheck={false}
                  placeholder="https://apps.apple.com/app/id…"
                  value={d.ios}
                  invalid={!!errorFor("ios_url")}
                  aria-describedby={desc}
                  onChange={(e) => set({ ios: e.target.value })}
                />
              )}
            </Field>
            <Field label="Android URL" hint="For Android phones. A Google Play link or a web address." error={errorFor("android_url")}>
              {(id, desc) => (
                <TextInput
                  id={id}
                  mono
                  inputMode="url"
                  autoComplete="off"
                  spellCheck={false}
                  placeholder="https://play.google.com/store/apps/details?id=…"
                  value={d.android}
                  invalid={!!errorFor("android_url")}
                  aria-describedby={desc}
                  onChange={(e) => set({ android: e.target.value })}
                />
              )}
            </Field>
            <Field
              wide
              label="App link"
              hint="Your app's own scheme. Phones try the app first and fall back to the web target."
              error={errorFor("deep_link")}
            >
              {(id, desc) => (
                <TextInput
                  id={id}
                  mono
                  autoComplete="off"
                  spellCheck={false}
                  placeholder="myapp://item/42"
                  value={d.deep}
                  invalid={!!errorFor("deep_link")}
                  aria-describedby={desc}
                  onChange={(e) => set({ deep: e.target.value })}
                />
              )}
            </Field>
          </div>
        </fieldset>

        {/* ── expiry ───────────────────────────────────────────────── */}
        <fieldset className="mf-gr-set">
          <legend className="mf-gr-set__title">Expiry</legend>
          <div className="mf-gr-grid">
            <Field
              label="Expires at"
              hint={`Leave empty for a link that never expires. The time is in ${localZone()}. After it, the short link shows an "expired" page.`}
              error={errorFor("expires_at")}
            >
              {(id, desc) => (
                <span className="mf-gr-inline">
                  <TextInput
                    id={id}
                    type="datetime-local"
                    mono
                    value={d.expires}
                    invalid={!!errorFor("expires_at")}
                    aria-describedby={desc}
                    onChange={(e) => set({ expires: e.target.value })}
                  />
                  {d.expires ? (
                    <Button size="sm" tone="quiet" onClick={() => set({ expires: "" })}>
                      Clear
                    </Button>
                  ) : null}
                </span>
              )}
            </Field>
          </div>
          {expiresPast ? (
            <Say tone="warn">This time has passed. The link is expired as soon as you save it.</Say>
          ) : null}
        </fieldset>

        <div className="mf-gr-form__foot">
          <Button type="submit" tone="primary" icon={<IconCheck size={13} />} disabled={save.isPending}>
            {save.isPending ? "Saving…" : link ? "Save changes" : "Create link"}
          </Button>
          <Button onClick={onCancel}>Cancel</Button>
          {formError ? <Say tone="warn">{formError}</Say> : null}
          {Object.keys(checked).length && !formError ? (
            <Say tone="warn">Fix the fields marked above, then save again.</Say>
          ) : null}
        </div>
      </form>
    </Panel>
  );
}
