/**
 * LOG DRAINS: AI crawler traffic from Vercel, with no change to the site.
 *
 * A site on Vercel has no access log to follow, so the log shipper on the
 * Tracking setup screen cannot reach it. Vercel can send its own request log
 * to an address instead, and that address is what this screen creates.
 *
 * The secret is shown exactly once, in the panel that appears after a drain is
 * created or its secret is rotated. The warning sits BEFORE the button that
 * issues it, as on the API keys screen, because by the time the panel appears
 * the choice of where to keep it has already been made. Micaforge stores the
 * secret sealed and never sends it back, so "you will not see it again" is a
 * fact about the server, not a policy.
 *
 * Rotating and deleting are confirmed by typing the drain's short name, inside
 * the page. There is no confirmation dialog.
 */
import { useState } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";

import { Button, CopyField, Disclosure, Empty, Panel, PanelHead } from "../../components/react/ui";
import { IconCheck, IconPlus } from "../../components/react/icons";
import { api } from "../../lib/api";
import { ago } from "../../lib/format";
import { useSite } from "../providers";
import {
  Block,
  Caution,
  Note,
  QueryState,
  Result,
  ResultFoot,
  Spec,
  SpecRow,
  Stated,
  Status,
  TextInput,
  TypedConfirm,
  Foot,
} from "./SettingsLayout";
import { useSiteRecord } from "./SiteSettings";

interface Drain {
  id: string;
  provider: string;
  /** Where Vercel sends the logs. Not a secret: the signature is. */
  url: string;
  verify_token: string | null;
  created_at: string;
  last_received_at: string | null;
}

interface IssuedDrain extends Drain {
  /** The only time the plaintext leaves the server. */
  secret: string;
  rotated_at?: string;
}

/**
 * The last eight characters of the id. The first characters of a time-ordered
 * id are a timestamp, so two drains made on the same day would share them.
 */
function shortName(id: string): string {
  return id.replace(/-/g, "").slice(-8);
}

const VERCEL_STEPS: { what: string; detail: string }[] = [
  { what: "Where", detail: "Vercel, Team Settings, Drains, then Add Drain." },
  { what: "Data type", detail: "Logs." },
  {
    what: "Sources",
    detail:
      "Every source that answers a request: static, lambda, edge, external, redirect and firewall. The build source adds nothing here.",
  },
  {
    what: "Environment",
    detail: "Production. A preview address is not one of this site's domains, so its requests are not recorded.",
  },
  { what: "Destination", detail: "Custom endpoint, with the drain URL as the endpoint URL." },
  { what: "Format", detail: "JSON or NDJSON. Both work." },
  {
    what: "Signature verification secret",
    detail: "Replace the secret Vercel generated with the one Micaforge showed you.",
  },
];

/* ── one drain ───────────────────────────────────────────────────────────── */

function DrainBlock({
  drain,
  siteRef,
  onIssued,
  onDeleted,
}: {
  drain: Drain;
  siteRef: string;
  onIssued: (d: IssuedDrain) => void;
  onDeleted: (id: string) => void;
}) {
  const qc = useQueryClient();
  const name = shortName(drain.id);
  const listKey = ["settings", "log-drains", siteRef];
  const [token, setToken] = useState(drain.verify_token ?? "");

  const saveToken = useMutation({
    mutationFn: (value: string) =>
      api
        .patch<Drain>(`/api/sites/${siteRef}/log-drains/${drain.id}`, { verify_token: value.trim() || null })
        .then((r) => r.data),
    onSuccess: () => qc.invalidateQueries({ queryKey: listKey }),
  });

  const rotate = useMutation({
    mutationFn: () =>
      api.post<IssuedDrain>(`/api/sites/${siteRef}/log-drains/${drain.id}/rotate`).then((r) => r.data),
    onSuccess: (d) => {
      onIssued(d);
      void qc.invalidateQueries({ queryKey: listKey });
    },
  });

  const remove = useMutation({
    mutationFn: () => api.del<null>(`/api/sites/${siteRef}/log-drains/${drain.id}`),
    onSuccess: () => {
      onDeleted(drain.id);
      void qc.invalidateQueries({ queryKey: listKey });
    },
  });

  const tokenDirty = token.trim() !== (drain.verify_token ?? "");

  return (
    <div className="mf-set-intake">
      <div className="mf-set-intake__head">
        <h3 className="mf-set-intake__name">Vercel drain {name}</h3>
        <span className="mf-set-intake__catch">Created {ago(drain.created_at)}</span>
      </div>
      <div className="mf-set-intake__body">
        <div className="mf-set-row">
          {drain.last_received_at ? (
            <Status state="done" label={`Last delivery ${ago(drain.last_received_at)}`} />
          ) : (
            <Status state="waiting" label="No delivery received yet" />
          )}
        </div>

        <CopyField label="Drain URL" value={drain.url} />

        <Spec>
          <SpecRow
            name="Ownership token"
            hint="Only if Vercel asks you to verify the URL. Paste the token it shows, and Micaforge returns it in the x-vercel-verify header."
            htmlFor={`drain-token-${drain.id}`}
          >
            <div className="mf-set-row">
              <TextInput
                id={`drain-token-${drain.id}`}
                data
                spellCheck={false}
                autoComplete="off"
                placeholder="Not needed for most drains"
                value={token}
                onChange={(e) => setToken(e.target.value)}
              />
              <Button
                size="sm"
                icon={<IconCheck size={12} />}
                disabled={!tokenDirty || saveToken.isPending}
                onClick={() => saveToken.mutate(token)}
              >
                {token.trim() ? "Save the token" : "Clear the token"}
              </Button>
            </div>
          </SpecRow>
        </Spec>
        <ResultFoot action={saveToken} done="Token saved." working="Saving…" />

        <Disclosure summary="Rotate the secret">
          <TypedConfirm
            phrase={name}
            label="Rotate the secret"
            busy={rotate.isPending}
            destroys={
              <>
                The current secret stops working the moment the new one is issued. Vercel&apos;s deliveries are
                refused until you paste the new secret into the drain in Vercel. Nothing already recorded is
                affected.
              </>
            }
            onConfirm={() => rotate.mutate()}
          />
          <div className="mf-set-row">
            <Result action={rotate} done="New secret issued. Copy it above." working="Rotating…" />
          </div>
        </Disclosure>

        <Disclosure summary="Delete this drain">
          <TypedConfirm
            phrase={name}
            label="Delete the drain"
            busy={remove.isPending}
            destroys={
              <>
                Micaforge stops accepting deliveries to this URL immediately, and the URL cannot be used again.
                AI crawler traffic already recorded from it stays. Delete the drain in Vercel as well, or Vercel
                will report every delivery as failed.
              </>
            }
            onConfirm={() => remove.mutate()}
          />
          <div className="mf-set-row">
            <Result action={remove} done="Drain deleted." working="Deleting…" />
          </div>
        </Disclosure>
      </div>
    </div>
  );
}

/* ── the screen ──────────────────────────────────────────────────────────── */

export function LogDrainsSettings() {
  const { domain } = useSite();
  const { id: siteRef } = useSiteRecord();
  const qc = useQueryClient();
  const listKey = ["settings", "log-drains", siteRef];
  const [issued, setIssued] = useState<IssuedDrain | null>(null);
  const [newToken, setNewToken] = useState("");

  // A secret for a drain that no longer exists is no use to anyone.
  const forgetIssued = (id: string) => setIssued((cur) => (cur?.id === id ? null : cur));

  const drains = useQuery({
    queryKey: listKey,
    queryFn: () => api.get<Drain[]>(`/api/sites/${siteRef}/log-drains`).then((r) => r.data),
    enabled: !!siteRef,
  });

  const create = useMutation({
    mutationFn: () =>
      api
        .post<IssuedDrain>(`/api/sites/${siteRef}/log-drains`, {
          provider: "vercel",
          verify_token: newToken.trim() || null,
        })
        .then((r) => r.data),
    onSuccess: (d) => {
      setIssued(d);
      setNewToken("");
      void qc.invalidateQueries({ queryKey: listKey });
    },
  });

  return (
    <>
      {issued ? (
        <Panel>
          <PanelHead title="Copy the secret now" />
          <Block
            title={`Vercel drain ${shortName(issued.id)}`}
            body="You will not see this secret again. Micaforge stores it sealed and never sends it back, so a lost secret is replaced by rotating it, not recovered. Paste both values into the drain form in Vercel."
          >
            <div className="mf-set-reveal">
              <CopyField label="Endpoint URL" value={issued.url} />
              <CopyField label="Signature verification secret" value={issued.secret} />
            </div>
            <div className="mf-set-row">
              <Button tone="primary" onClick={() => setIssued(null)}>
                I have copied it
              </Button>
              <Note>Closing this panel is the last chance to copy the secret.</Note>
            </div>
          </Block>
        </Panel>
      ) : null}

      <Panel>
        <PanelHead
          title="Log drains"
          note="Vercel sends its request log to these addresses. Micaforge records the AI crawler requests in it, with no change to your site's code."
        />
        <QueryState
          query={drains}
          emptyWhen={(rows) => rows.length === 0}
          empty={
            <Empty
              title="No log drain exists"
              body={`If ${domain || "this site"} is hosted on Vercel, create a drain below. Vercel then sends each request it answers, and the AI crawler requests appear on the AI crawlers screen.`}
            />
          }
        >
          {(rows) => (
            <>
              {rows.map((d) => (
                <DrainBlock
                  key={d.id}
                  drain={d}
                  siteRef={siteRef}
                  onIssued={setIssued}
                  onDeleted={forgetIssued}
                />
              ))}
            </>
          )}
        </QueryState>
      </Panel>

      <Panel>
        <PanelHead
          title="Add a Vercel drain"
          note="Use this when the site is served by Vercel. Sites on your own server use the log shipper on the Tracking setup screen instead."
        />
        <Spec>
          <SpecRow
            name="Ownership token"
            hint="Optional. Only if Vercel asks you to verify the URL. You can add it later."
            htmlFor="drain-new-token"
          >
            <TextInput
              id="drain-new-token"
              data
              spellCheck={false}
              autoComplete="off"
              placeholder="Leave empty unless Vercel asks"
              value={newToken}
              onChange={(e) => setNewToken(e.target.value)}
            />
          </SpecRow>
        </Spec>
        <Foot>
          <Caution>
            The secret is shown once, on this screen, immediately after the drain is created. Open the drain form
            in Vercel before you press this.
          </Caution>
        </Foot>
        <Foot tight>
          <Button
            tone="primary"
            icon={<IconPlus size={14} />}
            disabled={!siteRef || create.isPending}
            onClick={() => create.mutate()}
          >
            Create the drain
          </Button>
          <span className="mf-set-foot__spacer" />
          <Result action={create} done="Drain created. Copy the secret above." working="Creating…" />
        </Foot>
      </Panel>

      <Panel>
        <PanelHead
          title="Settings in Vercel"
          note="Log drains need a Vercel Pro or Enterprise plan. Fill in the drain form in Vercel like this."
        />
        <div className="mf-set-block">
          <div className="mf-set-ledger">
            {VERCEL_STEPS.map((s) => (
              <div className="mf-set-ledger__row" key={s.what}>
                <span className="mf-set-ledger__tick" aria-hidden="true" />
                <span>
                  <span className="mf-set-ledger__what">{s.what}</span>: {s.detail}
                </span>
              </div>
            ))}
          </div>
        </div>
        <Spec>
          <SpecRow
            name="What is recorded"
            hint="Requests made with GET or HEAD. Other methods, build output and the lines your functions print are skipped."
          >
            <Stated>
              AI crawlers and other automated clients. Visits from people are skipped, because the tracker on
              your pages already counts them.
            </Stated>
          </SpecRow>
        </Spec>
      </Panel>
    </>
  );
}

export default LogDrainsSettings;
