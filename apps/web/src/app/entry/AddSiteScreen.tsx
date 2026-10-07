/**
 * Add a site.
 *
 * Three facts and nothing else: the domain to record, a name to call it, and
 * the timezone its days roll over in. The domain is normalised and validated in
 * the browser so a paste of a full URL is fixed rather than rejected, and what
 * will actually be recorded is stated back before anything is created.
 */
import type { FormEvent } from "react";
import { useMemo, useState } from "react";
import { useNavigate } from "react-router";
import { useMutation, useQueryClient } from "@tanstack/react-query";
import { Button, Panel, PanelHead } from "../../components/react/ui";
import type { SiteSummary } from "../../lib/api";
import { api } from "../../lib/api";
import { ENTRY_PATHS, Field, FormError, Select, describedBy, fieldError, formError } from "./AuthFrame";
import "../../styles/app/entry.css";

/* ── the domain ──────────────────────────────────────────────────────────── */

const LABEL = /^[\p{L}\p{N}](?:[\p{L}\p{N}-]{0,61}[\p{L}\p{N}])?$/u;

export interface DomainReading {
  domain: string;
  error?: string;
}

/**
 * Takes whatever was pasted and reads a bare hostname out of it. A full URL, a
 * trailing slash, a scheme, a path: all of these are things a person genuinely
 * types, and none of them is a reason to refuse the form.
 */
export function readDomain(raw: string): DomainReading {
  let v = raw.trim().toLowerCase();
  if (!v) return { domain: "", error: "Enter the domain you want to record." };

  v = v.replace(/^[a-z][a-z0-9+.-]*:\/\//, ""); // scheme
  v = v.replace(/^[^/@]*@/, ""); // credentials
  v = v.split(/[/?#]/)[0]; // path, query, fragment
  v = v.replace(/\.$/, ""); // the root dot

  if (!v) return { domain: "", error: "Enter the domain you want to record." };
  if (v.includes(":")) return { domain: v.split(":")[0], error: "Leave the port out. Record the domain only." };
  if (v.includes(" ")) return { domain: v, error: "A domain has no spaces in it." };
  if (v.length > 253) return { domain: v, error: "That is longer than a domain name can be." };

  const labels = v.split(".");
  if (labels.length < 2)
    return { domain: v, error: "Give the full domain, including the ending. For example, example.com." };
  if (/^[\d.]+$/.test(v)) return { domain: v, error: "Record a domain name rather than an IP address." };
  for (const l of labels) {
    if (!LABEL.test(l))
      return {
        domain: v,
        error: "Use letters, numbers and hyphens between the dots. For example, docs.example.com.",
      };
  }
  return { domain: v };
}

/* ── the timezone ────────────────────────────────────────────────────────── */

const FALLBACK_ZONES = [
  "UTC",
  "Europe/London",
  "Europe/Lisbon",
  "Europe/Madrid",
  "Europe/Paris",
  "Europe/Berlin",
  "Europe/Warsaw",
  "Europe/Kyiv",
  "Africa/Lagos",
  "Africa/Johannesburg",
  "Asia/Jerusalem",
  "Asia/Dubai",
  "Asia/Kolkata",
  "Asia/Singapore",
  "Asia/Shanghai",
  "Asia/Tokyo",
  "Australia/Sydney",
  "Pacific/Auckland",
  "America/Sao_Paulo",
  "America/New_York",
  "America/Chicago",
  "America/Denver",
  "America/Los_Angeles",
];

function browserZone(): string {
  try {
    return Intl.DateTimeFormat().resolvedOptions().timeZone || "UTC";
  } catch {
    return "UTC";
  }
}

function allZones(): string[] {
  try {
    const supported = (Intl as unknown as { supportedValuesOf?: (k: string) => string[] }).supportedValuesOf;
    if (typeof supported === "function") {
      const list = supported.call(Intl, "timeZone");
      if (Array.isArray(list) && list.length) return list;
    }
  } catch {
    /* an engine without the Intl extension falls back to the short list */
  }
  return FALLBACK_ZONES;
}

/* ── the screen ──────────────────────────────────────────────────────────── */

export interface AddSiteScreenProps {
  /** Where a freshly created site goes. Onboarding, unless the router says otherwise. */
  nextPath?: (siteId: number) => string;
}

export function AddSiteScreen({ nextPath = ENTRY_PATHS.onboarding }: AddSiteScreenProps) {
  const navigate = useNavigate();
  const queryClient = useQueryClient();

  const [domainInput, setDomainInput] = useState("");
  const [name, setName] = useState("");
  const [timezone, setTimezone] = useState(browserZone);
  const [touched, setTouched] = useState(false);

  const zones = useMemo(() => {
    const list = allZones();
    const mine = browserZone();
    return list.includes(mine) ? list : [mine, ...list];
  }, []);

  const reading = readDomain(domainInput);
  const showDomainError = touched && domainInput.trim().length > 0 ? reading.error : undefined;

  const create = useMutation({
    mutationFn: (body: { domain: string; name: string; timezone: string }) =>
      api.post<SiteSummary>("/api/sites", body).then((r) => r.data),
    onSuccess: async (site) => {
      await queryClient.invalidateQueries({ queryKey: ["sites"] });
      navigate(nextPath(site.site_id), { replace: true });
    },
  });

  const busy = create.isPending;
  const err = create.error;

  const submit = (e: FormEvent) => {
    e.preventDefault();
    setTouched(true);
    if (busy || reading.error) return;
    create.mutate({
      domain: reading.domain,
      name: name.trim() || reading.domain,
      timezone,
    });
  };

  return (
    <div className="mf-stack mf-stack--narrow">
      <div className="mf-chooser__mast">
        <div>
          <h1 className="mf-chooser__title">Add a site</h1>
          <p className="mf-chooser__sub">
            A site is one domain you record, and everything under it. Subdomains are separate sites
            unless you point their tracker at this one, so docs.example.com and example.com are two
            records by default.
          </p>
        </div>
      </div>

      <Panel ruled feed>
        <PanelHead
          title="What are we recording?"
          note="You can change the name and the timezone later. The domain is what the tracker checks against, so get that one right."
        />

        <form className="mf-form mf-panel-form" onSubmit={submit} noValidate>
          {formError(err) ? <FormError>{formError(err)}</FormError> : null}

          <div className="mf-form__fields">
            <Field
              label="Domain"
              htmlFor="mf-site-domain"
              error={showDomainError ?? fieldError(err, "domain")}
              hint={
                reading.domain && !reading.error
                  ? `Micaforge will record ${reading.domain} and every path under it.`
                  : "Paste a full address if that is easier and Micaforge will take the domain out of it. For example, example.com."
              }
            >
              <input
                id="mf-site-domain"
                className="mf-input mf-input--data"
                type="text"
                inputMode="url"
                autoComplete="off"
                autoCapitalize="off"
                spellCheck={false}
                value={domainInput}
                disabled={busy}
                required
                aria-invalid={showDomainError || fieldError(err, "domain") ? true : undefined}
                aria-describedby={describedBy("mf-site-domain", {
                  error: showDomainError ?? fieldError(err, "domain"),
                  hint: true,
                })}
                onChange={(e) => setDomainInput(e.target.value)}
                onBlur={() => setTouched(true)}
              />
            </Field>

            <Field
              label="Name"
              htmlFor="mf-site-name"
              error={fieldError(err, "name")}
              hint={
                reading.domain && !reading.error
                  ? `What this site is called in your list. Left empty it is called ${reading.domain}.`
                  : "What this site is called in your list. Left empty it takes the domain."
              }
            >
              <input
                id="mf-site-name"
                className="mf-input"
                type="text"
                autoComplete="off"
                value={name}
                disabled={busy}
                aria-invalid={fieldError(err, "name") ? true : undefined}
                aria-describedby={describedBy("mf-site-name", { error: fieldError(err, "name"), hint: true })}
                onChange={(e) => setName(e.target.value)}
              />
            </Field>

            <Field
              label="Timezone"
              htmlFor="mf-site-tz"
              error={fieldError(err, "timezone")}
              hint="Days, weeks and the daily visitor hash all roll over at midnight in this zone."
            >
              <Select
                id="mf-site-tz"
                value={timezone}
                onChange={setTimezone}
                disabled={busy}
                invalid={!!fieldError(err, "timezone")}
                describedBy={describedBy("mf-site-tz", { error: fieldError(err, "timezone"), hint: true })}
              >
                {zones.map((z) => (
                  <option key={z} value={z}>
                    {z.replace(/_/g, " ")}
                  </option>
                ))}
              </Select>
            </Field>
          </div>

          <div className="mf-form__foot">
            <Button tone="primary" type="submit" disabled={busy}>
              {busy ? "Adding…" : "Add the site"}
            </Button>
            <Button tone="quiet" onClick={() => navigate(ENTRY_PATHS.chooser)} disabled={busy}>
              Cancel
            </Button>
          </div>
        </form>
      </Panel>
    </div>
  );
}

export default AddSiteScreen;
