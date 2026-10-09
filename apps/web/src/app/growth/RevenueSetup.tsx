/**
 * Connecting Stripe or Paddle, and the connections a site already has.
 *
 * The order of the steps is forced by the providers, not chosen here. Both
 * Stripe and Paddle show the secret that signs their events only AFTER they
 * have been given the address to send them to. So a connection is made in two
 * moves:
 *
 *   1. "Create webhook address" makes the connection with no secret. The server
 *      answers with the address, and the card shows it with the exact events to
 *      subscribe to.
 *   2. The operator adds the address in the provider, copies the secret the
 *      provider then shows, and pastes it here. Until then the card says
 *      "Waiting for the signing secret" in words, and the server refuses the
 *      connection's webhooks.
 *
 * The secret is typed into a password field and is never shown again, by this
 * screen or by the server. Replacing it later is the same field.
 *
 * Changing a connection needs the admin role. A refusal is written out as
 * that, in words, rather than as a bare "forbidden".
 */
import type { SubmitEvent } from "react";
import { useEffect, useRef, useState } from "react";
import { useMutation, useQueryClient } from "@tanstack/react-query";

import { Button, CopyField, Disclosure, Panel, PanelHead } from "../../components/react/ui";
import { IconCheck, IconPlus } from "../../components/react/icons";
import { api, publicOrigin } from "../../lib/api";
import { ago } from "../../lib/format";
import { Bank, Field, Say, Switch, TextInput, WaitLines } from "./controls";
import { PROVIDERS, adminProblem, day, moment, problem } from "./model";
import type { Integration, Provider } from "./model";

const ALL_PROVIDERS: Provider[] = ["stripe", "paddle"];

/* ── step 1: make the address ────────────────────────────────────────────── */

export function ConnectProvider({
  siteId,
  taken,
  lead,
  onCreated,
}: {
  siteId: number;
  /** Providers this site already has; each provider is connected once. */
  taken: Provider[];
  /** The one committed action on the screen, so it takes the primary tone. */
  lead?: boolean;
  onCreated: (id: string) => void;
}) {
  const qc = useQueryClient();
  const free = ALL_PROVIDERS.filter((p) => !taken.includes(p));
  const [chosen, setChosen] = useState<Provider>(free[0] ?? "stripe");
  const provider = free.includes(chosen) ? chosen : (free[0] ?? chosen);
  const name = PROVIDERS[provider].name;

  const create = useMutation({
    mutationFn: () =>
      api
        .post<Integration>(`/api/revenue/integrations?site=${encodeURIComponent(String(siteId))}`, { provider })
        .then((r) => r.data),
    onSuccess: (made) => {
      void qc.invalidateQueries({ queryKey: ["revenue"] });
      onCreated(made.id);
    },
  });

  if (!free.length) return null;

  return (
    <div className="mf-gr-connect">
      {free.length > 1 ? (
        <Bank<Provider>
          label="Payment provider"
          value={provider}
          options={free.map((p) => ({ v: p, label: PROVIDERS[p].name }))}
          onChange={(p) => {
            setChosen(p);
            if (create.isError) create.reset();
          }}
        />
      ) : null}
      <p className="mf-gr-connect__text">
        Micaforge makes a webhook address for {name}. You add that address in {name}, and {name} then shows the
        secret that signs its events. You paste the secret here in the next step.
      </p>
      <div className="mf-gr-inline">
        <Button
          tone={lead ? "primary" : "key"}
          icon={<IconPlus size={13} />}
          disabled={create.isPending}
          onClick={() => create.mutate()}
        >
          {create.isPending ? "Creating…" : `Create ${name} webhook address`}
        </Button>
        <a className="mf-gr-link" href={PROVIDERS[provider].docs}>
          Read the {name} setup guide
        </a>
      </div>
      {create.isError ? <Say tone="warn">{adminProblem(create.error)}</Say> : null}
    </div>
  );
}

/* ── step 2: the secret ──────────────────────────────────────────────────── */

function SecretForm({
  item,
  onDone,
  onCancel,
}: {
  item: Integration;
  onDone: () => void;
  onCancel?: () => void;
}) {
  const qc = useQueryClient();
  const p = PROVIDERS[item.provider];
  const [secret, setSecret] = useState("");

  const save = useMutation({
    mutationFn: (value: string) =>
      api
        .patch<Integration>(`/api/revenue/integrations/${encodeURIComponent(item.id)}`, { secret: value })
        .then((r) => r.data),
    onSuccess: () => {
      setSecret("");
      void qc.invalidateQueries({ queryKey: ["revenue"] });
      onDone();
    },
  });

  const submit = (e: SubmitEvent) => {
    e.preventDefault();
    const v = secret.trim();
    if (v) save.mutate(v);
  };

  return (
    <form className="mf-gr-secret" onSubmit={submit}>
      <Field label={p.secretName} hint={p.secretHint} error={save.isError ? adminProblem(save.error) : null}>
        {(id, desc) => (
          <TextInput
            id={id}
            type="password"
            mono
            autoComplete="new-password"
            spellCheck={false}
            value={secret}
            invalid={save.isError}
            aria-describedby={desc}
            onChange={(e) => {
              setSecret(e.target.value);
              if (save.isError) save.reset();
            }}
          />
        )}
      </Field>
      <div className="mf-gr-inline">
        <Button
          type="submit"
          tone={item.has_secret ? "key" : "primary"}
          icon={<IconCheck size={13} />}
          disabled={!secret.trim() || save.isPending}
        >
          {save.isPending ? "Saving…" : item.has_secret ? "Replace secret" : "Save secret"}
        </Button>
        {onCancel ? (
          <Button tone="quiet" onClick={onCancel}>
            Cancel
          </Button>
        ) : null}
      </div>
    </form>
  );
}

/* ── one connection ──────────────────────────────────────────────────────── */

function stateWords(item: Integration): { text: string; tone?: "warn" | "good" } {
  if (!item.has_secret) return { text: "Waiting for the signing secret", tone: "warn" };
  if (!item.enabled) return { text: "Off. New events are not recorded." };
  if (!item.last_event_at) return { text: "Waiting for the first event" };
  return { text: `Last event ${ago(item.last_event_at)}`, tone: "good" };
}

function Steps({ item }: { item: Integration }) {
  const p = PROVIDERS[item.provider];
  return (
    <>
      <CopyField label={`Webhook address to add in ${p.name}`} value={item.webhook_url} />
      <div className="mf-gr-events">
        <span className="mf-gr-events__label">Events to send</span>
        <ul className="mf-gr-events__list">
          {p.events.map((e) => (
            <li key={e}>
              <code className="mf-data">{e}</code>
            </li>
          ))}
        </ul>
      </div>
    </>
  );
}

function IntegrationCard({ item, focus }: { item: Integration; focus: boolean }) {
  const qc = useQueryClient();
  const p = PROVIDERS[item.provider];
  const [replacing, setReplacing] = useState(false);
  const [saved, setSaved] = useState(false);
  const [asking, setAsking] = useState(false);
  const headRef = useRef<HTMLHeadingElement>(null);

  useEffect(() => {
    if (focus) headRef.current?.focus();
  }, [focus]);

  const toggle = useMutation({
    mutationFn: (enabled: boolean) =>
      api.patch<Integration>(`/api/revenue/integrations/${encodeURIComponent(item.id)}`, { enabled }),
    onSuccess: () => void qc.invalidateQueries({ queryKey: ["revenue"] }),
  });
  const remove = useMutation({
    mutationFn: () => api.del<{ deleted: boolean }>(`/api/revenue/integrations/${encodeURIComponent(item.id)}`),
    onSuccess: () => void qc.invalidateQueries({ queryKey: ["revenue"] }),
  });

  const state = stateWords(item);
  const settled = item.has_secret && item.last_event_at != null;

  return (
    <article className="mf-gr-int" aria-labelledby={`mf-gr-int-${item.id}`}>
      <header className="mf-gr-int__head">
        <h3 ref={headRef} id={`mf-gr-int-${item.id}`} tabIndex={-1} className="mf-gr-int__name">
          {p.name}
        </h3>
        <span className="mf-gr-state" data-tone={state.tone}>
          {state.text}
        </span>
        <span className="mf-gr-int__switch">
          <Switch
            checked={item.enabled}
            label={`Record payments from ${p.name}`}
            disabled={toggle.isPending}
            onChange={(v) => toggle.mutate(v)}
          />
        </span>
      </header>

      {settled ? (
        <Disclosure summary={`Webhook address and events for ${p.name}`}>
          <Steps item={item} />
        </Disclosure>
      ) : (
        <ol className="mf-gr-steps">
          <li className="mf-gr-steps__item">
            <span className="mf-gr-steps__title">In {p.name}, add this address and choose these events</span>
            <Steps item={item} />
          </li>
          <li className="mf-gr-steps__item">
            <span className="mf-gr-steps__title">
              {item.has_secret ? "Signing secret saved" : `Paste the ${p.secretName.toLowerCase()} ${p.name} shows you`}
            </span>
            {item.has_secret ? (
              <p className="mf-gr-int__text">
                Saved. It is never shown again. {p.name} can now send events; the first one appears here as soon as
                it arrives.
              </p>
            ) : (
              <SecretForm item={item} onDone={() => setSaved(false)} />
            )}
          </li>
        </ol>
      )}

      {item.has_secret && replacing ? (
        <SecretForm
          item={item}
          onDone={() => {
            setReplacing(false);
            setSaved(true);
          }}
          onCancel={() => setReplacing(false)}
        />
      ) : null}

      {saved && settled && !replacing ? <Say tone="good">Secret replaced.</Say> : null}
      {toggle.isError ? <Say tone="warn">{adminProblem(toggle.error)}</Say> : null}

      <footer className="mf-gr-int__foot">
        <span className="mf-gr-int__when">
          Connected {day(item.created_at)}
          {item.last_event_at ? ` · last event ${moment(item.last_event_at)}` : ""}
        </span>
        <span className="mf-gr-int__acts">
          <a className="mf-gr-link" href={p.docs}>
            {p.name} setup guide
          </a>
          {item.has_secret && !replacing ? (
            <Button size="sm" onClick={() => setReplacing(true)}>
              Replace secret
            </Button>
          ) : null}
          {asking ? null : (
            <Button size="sm" tone="danger" onClick={() => setAsking(true)}>
              Delete
            </Button>
          )}
        </span>
      </footer>

      {asking ? (
        <div className="mf-gr-ask" role="group" aria-label={`Confirm deleting the ${p.name} connection`}>
          <p className="mf-gr-ask__text">
            Delete the {p.name} connection? Events sent to its address are refused from then on. Delete the
            endpoint in {p.name} too, or {p.name} keeps retrying.
          </p>
          <div className="mf-gr-ask__acts">
            <Button tone="danger" size="sm" disabled={remove.isPending} onClick={() => remove.mutate()}>
              {remove.isPending ? "Deleting…" : "Delete connection"}
            </Button>
            <Button size="sm" onClick={() => setAsking(false)}>
              Keep it
            </Button>
          </div>
          {remove.isError ? <Say tone="warn">{adminProblem(remove.error)}</Say> : null}
        </div>
      ) : null}
    </article>
  );
}

/* ── the connections panel ───────────────────────────────────────────────── */

export function Connections({
  siteId,
  items,
  loading,
  error,
  onRetry,
  focusId,
  onCreated,
}: {
  siteId: number;
  items: Integration[];
  loading: boolean;
  error: unknown;
  onRetry: () => void;
  focusId: string | null;
  onCreated: (id: string) => void;
}) {
  const taken = items.map((i) => i.provider);
  return (
    <Panel className="mf-gr-conns">
      <PanelHead
        title="Payment providers"
        note="Stripe and Paddle send each payment and refund to Micaforge by webhook. Changing a connection needs the admin role."
      />
      <div className="mf-gr-pad">
        {loading ? (
          <WaitLines lines={3} label="Loading connections…" />
        ) : error ? (
          <div className="mf-gr-inline">
            <Say tone="warn">Could not load the connections. {problem(error)}</Say>
            <Button size="sm" onClick={onRetry}>
              Try again
            </Button>
          </div>
        ) : (
          <>
            {items.map((i) => (
              <IntegrationCard key={i.id} item={i} focus={i.id === focusId} />
            ))}
            {taken.length < ALL_PROVIDERS.length ? (
              <div className="mf-gr-conns__more">
                <h3 className="mf-gr-conns__title">
                  {items.length ? "Connect another provider" : "Connect a provider"}
                </h3>
                <ConnectProvider siteId={siteId} taken={taken} onCreated={onCreated} />
              </div>
            ) : null}
          </>
        )}
      </div>
    </Panel>
  );
}

/* ── how a payment finds its visit ───────────────────────────────────────── */

export function AttributionNote({ siteId }: { siteId: number }) {
  return (
    <Panel className="mf-gr-about">
      <div className="mf-gr-pad">
        <Disclosure summary="How attribution works">
          <ol className="mf-gr-about__list">
            <li>
              <strong>Checkout reference.</strong> Before checkout, your page asks Micaforge for a reference, with{" "}
              <code className="mf-data">GET /api/revenue/ref?s={siteId}</code> or{" "}
              <code className="mf-data">checkoutRef()</code> from <code className="mf-data">@micaforge/sdk</code>.
              Pass it to the checkout: to Stripe as <code className="mf-data">client_reference_id</code> or the
              metadata key <code className="mf-data">mf_ref</code>, to Paddle as the custom data key{" "}
              <code className="mf-data">mf_ref</code>. The payment is then linked to the visit that started it, and
              later renewals and refunds for the same customer keep that link.
            </li>
            <li>
              <strong>Email.</strong> Without a reference, the customer&apos;s email is matched to people you
              identified with <code className="mf-data">identify()</code>.
            </li>
            <li>
              <strong>Neither.</strong> The payment counts as not attributed. It is still in the totals.
            </li>
          </ol>
          <CopyField
            label="Checkout reference address for this site"
            value={`${publicOrigin()}/api/revenue/ref?s=${siteId}`}
          />
        </Disclosure>
      </div>
    </Panel>
  );
}
