/**
 * GITHUB: deploys, releases and pushes from a site's repository, on its charts.
 *
 * Three people meet on this screen, and each gets the part that is theirs:
 *
 *   1. THE OPERATOR of this server creates the GitHub App once and sets its
 *      variables. Until then the screen says so, names the variables, and
 *      gives the two addresses the App needs, each with a Copy button.
 *   2. AN ORGANISATION ADMIN connects the App: Connect sends them to GitHub to
 *      install it on an account, and GitHub sends them back here. The server
 *      checks with GitHub that they can reach the installation they come back
 *      with, so nobody can attach somebody else's account.
 *   3. A SITE ADMIN picks the one repository this site follows, from the list
 *      GitHub gives for the installation, and chooses what becomes an
 *      annotation on the charts.
 *
 * Everybody who can read the site sees what was recorded. What arrived from
 * GitHub, and what became of each delivery, is shown to site admins, because
 * that is where a missing deploy is diagnosed.
 *
 * Annotations from GitHub are ordinary annotations, so the charts draw them
 * with no change; each one links to the release or commit it records.
 */
import { useMemo, useState } from "react";
import { useSearchParams } from "react-router";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";

import { Button, CopyField, Disclosure, Empty, Panel, PanelHead, Sheet, type Column } from "../../components/react/ui";
import { IconOut, IconPlus } from "../../components/react/icons";
import { api } from "../../lib/api";
import type {
  GitHubDelivery,
  GitHubEvent,
  GitHubEventKind,
  GitHubInstallation,
  GitHubLink,
  GitHubRepo,
  GitHubStatus,
} from "../../lib/api";
import { ago } from "../../lib/format";
import { useSite } from "../providers";
import {
  Block,
  Caution,
  CheckRow,
  Field,
  Foot,
  Note,
  QueryState,
  Result,
  ResultFoot,
  Select,
  Spec,
  SpecRow,
  Stated,
  Status,
  TextInput,
  Toggle,
  TypedConfirm,
  useSyncedForm,
  type StateName,
} from "./SettingsLayout";
import "../../styles/app/connect.css";

const APP_VARS = [
  "MICAFORGE_GITHUB_APP_ID",
  "MICAFORGE_GITHUB_APP_SLUG",
  "MICAFORGE_GITHUB_APP_PRIVATE_KEY",
  "MICAFORGE_GITHUB_APP_WEBHOOK_SECRET",
  "MICAFORGE_GITHUB_APP_CLIENT_ID",
  "MICAFORGE_GITHUB_APP_CLIENT_SECRET",
];

/** What GitHub's return to this screen said, by the code in the address. */
const RETURN_WORDS: Record<string, { tone: "good" | "warn"; text: string }> = {
  installed: { tone: "good", text: "The GitHub App is connected. Pick the repository for this site below." },
  requested: {
    tone: "good",
    text: "GitHub sent your request to an owner of the account. When they approve it, press Connect again to finish.",
  },
  state: { tone: "warn", text: "That connection request expired or was already used. Press Connect again." },
  signed_out: {
    tone: "warn",
    text: "You came back from GitHub signed out, or signed in as somebody else. Sign in, then press Connect again.",
  },
  denied: { tone: "warn", text: "The request was cancelled on GitHub. Nothing was connected." },
  forbidden: { tone: "warn", text: "Only an organisation admin can connect the GitHub App. Nothing was connected." },
  not_yours: {
    tone: "warn",
    text: "GitHub does not list that installation for your GitHub account, so it was not connected.",
  },
  github: {
    tone: "warn",
    text: "GitHub did not answer as expected, so nothing was connected. Try again. If it fails again, the server log names the step.",
  },
  disabled: { tone: "warn", text: "The GitHub App is not configured on this server." },
};

const KIND_WORDS: Record<GitHubEventKind, string> = {
  deploy: "Deploy",
  release: "Release",
  push: "Push",
};

const OUTCOME: Record<GitHubDelivery["outcome"], { state: StateName; label: string }> = {
  recorded: { state: "done", label: "Recorded" },
  ignored: { state: "waiting", label: "Ignored" },
  failed: { state: "failed", label: "Failed" },
  processing: { state: "running", label: "Processing" },
};

/** A link in ink with an outward arrow; never blue or orange. */
function OutLink({ to, children }: { to: string; children: React.ReactNode }) {
  return (
    <a className="mf-cx-link" href={to} target="_blank" rel="noreferrer noopener">
      {children}
      <IconOut size={12} />
    </a>
  );
}

/** The variable names, in the data face, joined as a sentence. */
function VarList({ names }: { names: string[] }) {
  return (
    <>
      {names.map((n, i) => (
        <span key={n}>
          {i > 0 ? (i === names.length - 1 ? " and " : ", ") : null}
          <code className="mf-data">{n}</code>
        </span>
      ))}
    </>
  );
}

/* ── what the App needs from GitHub ──────────────────────────────────────── */

const APP_SETTINGS: { what: string; detail: string }[] = [
  { what: "Callback URL", detail: "The callback address below, with “Request user authorization (OAuth) during installation” on." },
  { what: "Setup URL", detail: "Leave empty; with the option above on, GitHub uses the callback URL. Turn on “Redirect on update”." },
  { what: "Webhook", detail: "Active, with the webhook address below and a secret you choose." },
  { what: "Repository permissions", detail: "Contents: read. Deployments: read. Issues: read and write. Metadata: read." },
  { what: "Events", detail: "Push, Release, Deployment status. Installation and repository events arrive by themselves." },
];

function AppSetup({ status }: { status: GitHubStatus }) {
  return (
    <>
      <div className="mf-set-reveal">
        <CopyField label="Callback URL" value={status.callback_url} />
        <CopyField label="Webhook URL" value={status.webhook_url} />
      </div>
      <div className="mf-set-ledger">
        {APP_SETTINGS.map((s) => (
          <div className="mf-set-ledger__row" key={s.what}>
            <span className="mf-set-ledger__tick" aria-hidden="true" />
            <span>
              <span className="mf-set-ledger__what">{s.what}</span>: {s.detail}
            </span>
          </div>
        ))}
      </div>
      <div className="mf-cx-links">
        <OutLink to="/docs/integrations/github">Set up the GitHub App</OutLink>
      </div>
    </>
  );
}

/* ── the connection ──────────────────────────────────────────────────────── */

function InstallationRow({
  inst,
  canInstall,
  siteId,
}: {
  inst: GitHubInstallation;
  canInstall: boolean;
  siteId: number;
}) {
  const qc = useQueryClient();
  const remove = useMutation({
    mutationFn: () =>
      api
        .del<{ removed: boolean; uninstalled_on_github: boolean }>(`/api/github/installations/${inst.id}`)
        .then((r) => r.data),
    onSuccess: () => qc.invalidateQueries({ queryKey: ["github", "status", siteId] }),
  });

  return (
    <div className="mf-set-intake">
      <div className="mf-set-intake__head">
        <h3 className="mf-set-intake__name">
          <span className="mf-data">{inst.account}</span>
        </h3>
        <span className="mf-set-intake__catch">
          {inst.account_type === "Organization" ? "GitHub organisation" : "GitHub account"} ·{" "}
          {inst.repository_selection === "all" ? "every repository" : "chosen repositories"} · connected{" "}
          {ago(inst.created_at)}
        </span>
      </div>
      <div className="mf-set-intake__body">
        <div className="mf-set-row">
          {inst.suspended ? (
            <Status state="failed" label="Suspended on GitHub: nothing is recorded" />
          ) : (
            <Status state="done" label="Connected" />
          )}
        </div>
        {canInstall ? (
          <Disclosure summary="Disconnect this account">
            <TypedConfirm
              phrase={inst.account}
              label="Disconnect"
              busy={remove.isPending}
              destroys={
                <>
                  Every site of this organisation stops following its repositories on {inst.account}, and no new
                  deploys, releases or pushes are recorded from it. What was recorded, and its annotations, stay. When
                  no organisation on this server uses the installation any more, the App is uninstalled from{" "}
                  {inst.account} on GitHub too.
                </>
              }
              onConfirm={() => remove.mutate()}
            />
            <div className="mf-set-row">
              <Result
                action={remove}
                done={
                  remove.data?.uninstalled_on_github
                    ? "Disconnected, and uninstalled on GitHub."
                    : "Disconnected. The App stays installed on GitHub until you remove it there."
                }
                working="Disconnecting…"
              />
            </div>
          </Disclosure>
        ) : null}
      </div>
    </div>
  );
}

function Connection({ status, siteId }: { status: GitHubStatus; siteId: number }) {
  const connect = useMutation({
    mutationFn: () =>
      api.post<{ url: string }>(`/api/github/install?site=${encodeURIComponent(String(siteId))}`).then((r) => r.data),
    onSuccess: ({ url }) => {
      // GitHub's own page, in this tab: it sends the browser back here.
      window.location.assign(url);
    },
  });
  const many = status.installations.length > 0;

  return (
    <Panel>
      <PanelHead
        title="GitHub accounts"
        note="The GitHub accounts this organisation connected the App to. A site follows one repository from one of them."
      />
      {status.installations.map((inst) => (
        <InstallationRow key={inst.id} inst={inst} canInstall={status.can_install} siteId={siteId} />
      ))}
      {!many && status.can_install ? (
        <Empty
          title="No GitHub account is connected"
          body="Connect the App to the GitHub account or organisation that holds this site's repository. GitHub asks which repositories the App may read."
        />
      ) : null}
      {!many && !status.can_install ? (
        <Empty
          title="No GitHub account is connected"
          body="An admin of this organisation connects the App. Ask one to open this screen and press Connect."
        />
      ) : null}
      {status.can_install ? (
        <Foot>
          <Button
            tone={many ? "key" : "primary"}
            icon={<IconPlus size={14} />}
            disabled={connect.isPending || connect.isSuccess}
            onClick={() => connect.mutate()}
          >
            {many ? "Connect another account" : "Connect GitHub"}
          </Button>
          <span className="mf-set-foot__spacer" />
          <Result action={connect} done="Opening GitHub…" working="Preparing…" />
        </Foot>
      ) : null}
    </Panel>
  );
}

/* ── the repository ──────────────────────────────────────────────────────── */

function RepoPicker({
  status,
  siteId,
  current,
}: {
  status: GitHubStatus;
  siteId: number;
  current: GitHubLink | null;
}) {
  const qc = useQueryClient();
  const usable = status.installations.filter((i) => !i.suspended);
  const [installation, setInstallation] = useState<string>(current?.installation ?? usable[0]?.id ?? "");
  const [filter, setFilter] = useState("");
  const [repo, setRepo] = useState<string>("");

  const repos = useQuery({
    queryKey: ["github", "repos", siteId, installation],
    queryFn: () =>
      api
        .get<{ repositories: GitHubRepo[]; truncated: boolean }>(
          `/api/github/repos?site=${encodeURIComponent(String(siteId))}&installation=${encodeURIComponent(installation)}`,
        )
        .then((r) => r.data),
    enabled: !!installation,
  });

  const shown = useMemo(() => {
    const all = repos.data?.repositories ?? [];
    const q = filter.trim().toLowerCase();
    return q ? all.filter((r) => r.full_name.toLowerCase().includes(q)) : all;
  }, [repos.data, filter]);

  const link = useMutation({
    mutationFn: () =>
      api
        .put<GitHubLink>(`/api/github/link?site=${encodeURIComponent(String(siteId))}`, { installation, repo })
        .then((r) => r.data),
    onSuccess: () => {
      void qc.invalidateQueries({ queryKey: ["github", "status", siteId] });
      void qc.invalidateQueries({ queryKey: ["github", "deliveries", siteId] });
    },
  });

  if (!usable.length) {
    return (
      <Note>
        {status.installations.length
          ? "Every connected account has the App suspended on GitHub. Lift the suspension there to pick a repository."
          : "Connect a GitHub account above first; its repositories are listed here."}
      </Note>
    );
  }

  return (
    <div className="mf-set-stack">
      <div className="mf-set-row">
        {usable.length > 1 ? (
          <Field label="Account" htmlFor="gh-installation">
            <Select
              id="gh-installation"
              value={installation}
              onChange={(e) => {
                setInstallation(e.target.value);
                setRepo("");
              }}
            >
              {usable.map((i) => (
                <option key={i.id} value={i.id}>
                  {i.account}
                </option>
              ))}
            </Select>
          </Field>
        ) : null}
        <Field label="Find a repository" htmlFor="gh-filter">
          <TextInput
            id="gh-filter"
            data
            spellCheck={false}
            autoComplete="off"
            placeholder="owner/name"
            value={filter}
            onChange={(e) => setFilter(e.target.value)}
          />
        </Field>
      </div>
      <QueryState
        query={repos}
        emptyWhen={(d) => d.repositories.length === 0}
        empty={
          <Empty
            title="The App can read no repository on this account"
            body="On GitHub, open the App's installation settings for this account and add the site's repository."
          />
        }
      >
        {(d) => (
          <>
            <Field
              label="Repository"
              htmlFor="gh-repo"
              hint={
                d.truncated
                  ? "Only the first thousand repositories are listed. Type in the box above to narrow the list."
                  : `${shown.length} of ${d.repositories.length} repositories.`
              }
            >
              <Select id="gh-repo" value={repo} onChange={(e) => setRepo(e.target.value)}>
                <option value="">Choose a repository</option>
                {shown.map((r) => (
                  <option key={r.id} value={r.full_name}>
                    {r.full_name}
                    {r.private ? " (private)" : ""}
                  </option>
                ))}
              </Select>
            </Field>
            <div className="mf-set-row">
              <Button tone="primary" disabled={!repo || link.isPending} onClick={() => link.mutate()}>
                {current ? "Follow this repository instead" : "Follow this repository"}
              </Button>
              <Result action={link} done="Linked." working="Checking the repository with GitHub…" />
            </div>
          </>
        )}
      </QueryState>
    </div>
  );
}

interface OptionsForm {
  deploys: boolean;
  releases: boolean;
  pushes: boolean;
  environments: string;
  agent_issues: boolean;
}

function LinkOptions({ link, siteId }: { link: GitHubLink; siteId: number }) {
  const qc = useQueryClient();
  const server = useMemo<OptionsForm>(
    () => ({
      deploys: link.deploys,
      releases: link.releases,
      pushes: link.pushes,
      environments: link.environments.join(", "),
      agent_issues: link.agent_issues,
    }),
    [link],
  );
  const { form, dirty, set, reset } = useSyncedForm(server);

  const save = useMutation({
    mutationFn: (f: OptionsForm) =>
      api
        .patch<GitHubLink>(`/api/github/link?site=${encodeURIComponent(String(siteId))}`, {
          deploys: f.deploys,
          releases: f.releases,
          pushes: f.pushes,
          environments: f.environments
            .split(",")
            .map((s) => s.trim())
            .filter(Boolean),
          agent_issues: f.agent_issues,
        })
        .then((r) => r.data),
    onSuccess: () => qc.invalidateQueries({ queryKey: ["github", "status", siteId] }),
  });

  if (!form) return null;
  return (
    <>
      <Spec>
        <SpecRow
          name="On the chart"
          hint="Each one is recorded either way, and the AI analyst can read it. These choose which become annotations."
        >
          <div className="mf-set-stack mf-set-stack--tight">
            <CheckRow
              checked={form.deploys}
              onChange={(v) => set({ deploys: v })}
              label="Successful deployments"
              hint="To the environments below."
            />
            <CheckRow checked={form.releases} onChange={(v) => set({ releases: v })} label="Published releases" />
            <CheckRow
              checked={form.pushes}
              onChange={(v) => set({ pushes: v })}
              label={`Pushes to ${link.repo.default_branch}`}
              hint="A busy branch can mean many notes a day."
            />
          </div>
        </SpecRow>
        <SpecRow
          name="Environments"
          htmlFor="gh-envs"
          hint="Deployments to these environments are recorded, without regard to case. Separate them with commas. Leave empty to record every environment."
        >
          <TextInput
            id="gh-envs"
            data
            spellCheck={false}
            autoComplete="off"
            placeholder="Every environment"
            value={form.environments}
            onChange={(e) => set({ environments: e.target.value })}
          />
        </SpecRow>
        <SpecRow
          name="Issues from agents"
          hint="Lets an agent with an API key or MCP connection that holds manage:write open its own feedback about this site as an issue in this repository. Site admins can always do it from the feedback list."
        >
          <Toggle
            checked={form.agent_issues}
            onChange={(v) => set({ agent_issues: v })}
            label="Let agents open issues from their feedback"
          />
        </SpecRow>
      </Spec>
      <Foot>
        <Button tone="primary" disabled={!dirty || save.isPending} onClick={() => save.mutate(form)}>
          Save
        </Button>
        <Button tone="quiet" disabled={!dirty || save.isPending} onClick={reset}>
          Discard changes
        </Button>
        <span className="mf-set-foot__spacer" />
        <Result action={save} done="Saved." working="Saving…" />
      </Foot>
    </>
  );
}

function Repository({ status, siteId, domain }: { status: GitHubStatus; siteId: number; domain?: string }) {
  const qc = useQueryClient();
  const link = status.link;
  const unlink = useMutation({
    mutationFn: () => api.del<{ unlinked: boolean }>(`/api/github/link?site=${encodeURIComponent(String(siteId))}`),
    onSuccess: () => qc.invalidateQueries({ queryKey: ["github", "status", siteId] }),
  });

  return (
    <Panel>
      <PanelHead
        title="Repository"
        note={`The one repository ${domain ?? "this site"} follows, and what from it becomes an annotation on the charts.`}
      />
      {link ? (
        <>
          <Spec>
            <SpecRow name="Following">
              <Stated>
                <OutLink to={link.repo.html_url}>{link.repo.full_name}</OutLink>
                {link.suspended ? (
                  <>
                    {" "}
                    <Status state="failed" label="Suspended on GitHub" />
                  </>
                ) : null}
              </Stated>
            </SpecRow>
            <SpecRow name="Default branch">
              <Stated>
                <code className="mf-data">{link.repo.default_branch}</code>
              </Stated>
            </SpecRow>
          </Spec>
          {status.can_link ? (
            <>
              <LinkOptions link={link} siteId={siteId} />
              <div className="mf-set-block">
                <Disclosure summary="Follow another repository">
                  <RepoPicker status={status} siteId={siteId} current={link} />
                </Disclosure>
                <Disclosure summary="Unlink this repository">
                  <TypedConfirm
                    phrase={link.repo.full_name}
                    label="Unlink"
                    busy={unlink.isPending}
                    destroys={
                      <>
                        No new deploys, releases or pushes from {link.repo.full_name} are recorded for this site. What
                        was recorded, and its annotations, stay.
                      </>
                    }
                    onConfirm={() => unlink.mutate()}
                  />
                </Disclosure>
              </div>
              <ResultFoot action={unlink} done="Unlinked." working="Unlinking…" />
            </>
          ) : (
            <Foot>
              <Note>Only a site admin changes what this site records from GitHub.</Note>
            </Foot>
          )}
        </>
      ) : status.can_link ? (
        <Block
          title="Choose the repository"
          body="The list comes from GitHub: it is every repository the App can read on the account."
        >
          <RepoPicker status={status} siteId={siteId} current={null} />
        </Block>
      ) : (
        <Empty
          title="No repository is linked"
          body="A site admin links this site's repository here. Its deploys and releases then appear on the charts."
        />
      )}
    </Panel>
  );
}

/* ── what arrived ────────────────────────────────────────────────────────── */

function Activity({ siteId, linked }: { siteId: number; linked: boolean }) {
  const events = useQuery({
    queryKey: ["github", "events", siteId],
    queryFn: () => api.get<GitHubEvent[]>(`/api/github/events?site=${encodeURIComponent(String(siteId))}`).then((r) => r.data),
  });

  const columns: Column<GitHubEvent>[] = [
    { key: "kind", head: "Kind", width: "11%", render: (e) => <span>{KIND_WORDS[e.kind]}</span> },
    {
      key: "what",
      head: "What",
      render: (e) => (
        <span title={e.title}>
          {e.title}
          {e.sha ? <span className="mf-data"> · {e.sha.slice(0, 7)}</span> : null}
        </span>
      ),
    },
    {
      key: "where",
      head: "Where",
      width: "16%",
      render: (e) => <span className="mf-data">{e.environment ?? e.ref ?? "—"}</span>,
    },
    {
      key: "chart",
      head: "On the chart",
      width: "12%",
      render: (e) => <span>{e.annotated ? "Yes" : "No"}</span>,
    },
    {
      key: "when",
      head: "When",
      numeric: true,
      width: "12%",
      render: (e) => (
        <span className="mf-data" title={new Date(e.occurred_at).toLocaleString()}>
          {ago(e.occurred_at)}
        </span>
      ),
    },
    {
      key: "open",
      head: <span className="mf-sr">On GitHub</span>,
      numeric: true,
      width: "1%",
      render: (e) => (
        <OutLink to={e.url}>
          <span className="mf-sr">Open {KIND_WORDS[e.kind].toLowerCase()} on GitHub</span>
          <span aria-hidden="true">Open</span>
        </OutLink>
      ),
    },
  ];

  return (
    <Panel>
      <PanelHead
        title="Recorded from GitHub"
        note="The latest deploys, releases and pushes recorded for this site, newest first."
      />
      <QueryState query={events}>
        {(rows) => (
          <Sheet
            rows={rows}
            columns={columns}
            getKey={(e) => e.id}
            empty={
              <Empty
                title="Nothing recorded yet"
                body={
                  linked
                    ? "Successful deployments, published releases and pushes to the default branch appear here when GitHub sends them."
                    : "Link a repository above. Its deploys, releases and pushes then appear here."
                }
              />
            }
          />
        )}
      </QueryState>
    </Panel>
  );
}

function Deliveries({ siteId }: { siteId: number }) {
  const deliveries = useQuery({
    queryKey: ["github", "deliveries", siteId],
    queryFn: () =>
      api.get<GitHubDelivery[]>(`/api/github/deliveries?site=${encodeURIComponent(String(siteId))}`).then((r) => r.data),
  });

  const columns: Column<GitHubDelivery>[] = [
    {
      key: "event",
      head: "Event",
      width: "22%",
      render: (d) => (
        <span className="mf-data">
          {d.event}
          {d.action ? `.${d.action}` : ""}
        </span>
      ),
    },
    {
      key: "outcome",
      head: "Outcome",
      width: "14%",
      render: (d) => <Status state={OUTCOME[d.outcome].state} label={OUTCOME[d.outcome].label} />,
    },
    { key: "detail", head: "What happened", render: (d) => <span>{d.detail ?? "—"}</span> },
    {
      key: "when",
      head: "Received",
      numeric: true,
      width: "12%",
      render: (d) => (
        <span className="mf-data" title={new Date(d.received_at).toLocaleString()}>
          {ago(d.received_at)}
        </span>
      ),
    },
  ];

  return (
    <Panel>
      <PanelHead
        title="Recent deliveries"
        note="What GitHub sent about this repository and its installation, and what became of each one. A failed delivery can be sent again with Redeliver on GitHub."
      />
      <QueryState query={deliveries}>
        {(rows) => (
          <Sheet
            rows={rows}
            columns={columns}
            getKey={(d) => d.id}
            empty={
              <Empty
                title="No delivery has arrived"
                body="When the repository has a push, a release or a deployment, GitHub sends it here within seconds. If nothing arrives, check the webhook in the App's settings on GitHub."
              />
            }
          />
        )}
      </QueryState>
    </Panel>
  );
}

/* ── the screen ──────────────────────────────────────────────────────────── */

export function GitHubSettings() {
  const { siteId, domain } = useSite();
  const [params, setParams] = useSearchParams();
  const returned = params.get("github") ?? params.get("github_error");
  const said = returned ? RETURN_WORDS[returned] : undefined;

  const status = useQuery({
    queryKey: ["github", "status", siteId],
    queryFn: () =>
      api.get<GitHubStatus>(`/api/github/status?site=${encodeURIComponent(String(siteId))}`).then((r) => r.data),
    enabled: siteId != null,
  });

  const dismiss = () => {
    const next = new URLSearchParams(params);
    next.delete("github");
    next.delete("github_error");
    setParams(next, { replace: true });
  };

  return (
    <>
      {said ? (
        <Panel>
          <Foot>
            <span className="mf-set-foot__say" data-tone={said.tone} role={said.tone === "warn" ? "alert" : "status"}>
              {said.text}
            </span>
            <span className="mf-set-foot__spacer" />
            <Button size="sm" tone="quiet" onClick={dismiss}>
              Dismiss
            </Button>
          </Foot>
        </Panel>
      ) : null}

      <QueryState query={status}>
        {(s) =>
          !s.configured ? (
            <Panel>
              <PanelHead
                title="GitHub"
                note="Deploys, releases and pushes from this site's repository, as annotations on its charts, and feedback opened as GitHub issues."
              />
              <Block
                title={
                  <span className="mf-cx-plat__head">
                    GitHub App
                    <Status state="waiting" label="Not configured on this server" />
                  </span>
                }
                body={
                  <>
                    The operator of this server creates a GitHub App and switches it on by setting{" "}
                    <VarList names={APP_VARS} />, then restarting it. Until then nothing is sent to GitHub and nothing
                    GitHub sends is acted on.
                  </>
                }
              >
                <AppSetup status={s} />
              </Block>
            </Panel>
          ) : siteId == null ? null : (
            <>
              <Connection status={s} siteId={siteId} />
              <Repository status={s} siteId={siteId} domain={domain} />
              <Activity siteId={siteId} linked={!!s.link} />
              {s.can_link ? <Deliveries siteId={siteId} /> : null}
              {s.can_install ? (
                <Panel>
                  <PanelHead
                    title="The App on GitHub"
                    note="For whoever manages the GitHub App: the settings it needs on GitHub."
                  />
                  <Block title="App settings" body={s.app ? <OutLink to={s.app.url}>{s.app.url}</OutLink> : undefined}>
                    <AppSetup status={s} />
                  </Block>
                </Panel>
              ) : null}
              {!s.can_link && !s.can_install ? (
                <Caution>Only admins change these settings. You can read what was recorded.</Caution>
              ) : null}
            </>
          )
        }
      </QueryState>
    </>
  );
}

export default GitHubSettings;
