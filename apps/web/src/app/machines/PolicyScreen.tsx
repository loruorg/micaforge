/**
 * Policy versus observed, at /app/:site/policy
 *
 * What the site SAYS, beside what the machines DID. The files are rendered as
 * the files, in the data face, with the group that decides a given agent lifted
 * out of the page and the deciding rule marked in the margin.
 *
 * The rule this screen will not bend: a violation is only ever reported when the
 * agent's identity was verified against the operator's published address ranges.
 * A user-agent string is a claim, and anybody can send any claim. Unverified
 * fetches are shown, counted and labelled, never accused.
 */
import { useEffect, useMemo, useState } from "react";
import { useMutation, useQuery, keepPreviousData } from "@tanstack/react-query";

import { api } from "../../lib/api";
import {
  Button,
  Disclosure,
  Empty,
  Panel,
  PanelHead,
  Sheet,
  VerifiedMark,
} from "../../components/react/ui";
import type { Column } from "../../components/react/ui";
import { IconAlert, IconCheck, IconClose, IconInfo, IconOut, IconPolicy } from "../../components/react/icons";
import { ago, num, shortPath } from "../../lib/format";
import { agentName, titleCase } from "../display";
import { Bench, Reading, VerificationCaveat, Waiting, errorText, useMachineEnv } from "./AgentDetail";
import type { MachineEnv, RobotsState, VerifiedState } from "./AgentDetail";

/* ── shapes ─────────────────────────────────────────────────────────────── */

/** Mirrors the `site_policy` row plus the per-agent join the contract describes. */
interface PolicyDoc {
  url: string;
  present: boolean;
  text: string | null;
  fetched_at: number | null;
}

/**
 * One row of `GET /api/policy`.
 *
 * These field names are the endpoint's, not a convenient renaming of them: the
 * screen previously declared a guessed shape and every row rendered as
 * "Unnamed client" because `label` and `agent_id` do not exist on the wire.
 */
interface PolicyAgentRow {
  /** Catalog slug. */
  agent: string;
  /** The operator's own spelling of the crawler's name. */
  display_name: string;
  operator: string;
  purpose: string;
  /** `true` allowed, `false` disallowed, `null` no rule answers this agent. */
  allowed: boolean | null;
  /** Whether a group in the file names this agent rather than catching it in `*`. */
  robots_rule_named: boolean;
  /** The token the file would have to name. */
  robots_token: string;
  listed_in_llms: boolean;
  observed_fetches: number;
  /** Of those, how many came from an address that verified. */
  verified_fetches: number;
  /** Fetches on paths this agent's own rule disallows. */
  violations: number;
  /** Of those, how many carried a verified identity. Only these are reportable. */
  verified_violations: number;
  docs_url: string | null;
  /** Whether the operator STATES it obeys robots.txt. The claim, not the behaviour. */
  respects_robots: boolean;
  last_seen: string | null;
}

/** How this site's robots.txt answers an agent, derived from `allowed`. */
function robotsState(r: PolicyAgentRow): RobotsState {
  if (r.allowed === true) return "allowed";
  if (r.allowed === false) return "disallowed";
  return "unlisted";
}

/**
 * An identity verdict for the row.
 *
 * A fetch counts as verified only when its source address matched the operator's
 * published ranges. With no verified fetches at all we say so rather than
 * implying the claim was tested and failed.
 */
function verifiedState(r: PolicyAgentRow): VerifiedState {
  if (r.verified_fetches > 0) return "verified";
  if (r.observed_fetches > 0) return "unverified";
  return "unverified";
}

interface PolicyResponse {
  robots: PolicyDoc;
  llms: PolicyDoc;
  agents: PolicyAgentRow[];
}

interface ViolationRow {
  /** Catalog slug. */
  agent: string;
  /** The operator's own spelling of the crawler's name. */
  display_name: string;
  operator: string;
  docs_url: string | null;
  /** Whether the operator STATES it honours robots.txt. Context, not the verdict. */
  respects_robots: boolean;
  /** The path fetched despite being disallowed. */
  path: string;
  fetches: number;
  /** Of those, how many came from an address the operator publishes. */
  verified_fetches: number;
  /** Distinct sources, as rotating salted digests. Never an address. */
  unique_sources: number;
  first_seen: string | null;
  last_seen: string | null;
  /** "violation" only when at least one fetch verified; otherwise "unverified". */
  compliance: "violation" | "unverified";
}

/** Site-level totals from `GET /api/policy/violations`. */
interface ViolationsSummary {
  fetches: number;
  verified_fetches: number;
  agents: number;
  paths: number;
}

interface RefetchJob {
  id?: string;
  status?: string;
  progress?: number | null;
}

/* ── the robots.txt reader ──────────────────────────────────────────────── */

interface RobotsRule {
  line: number;
  kind: "allow" | "disallow";
  path: string;
}
interface RobotsGroup {
  /** Case-folded product tokens this group answers for. */
  tokens: string[];
  /** Every line index (0-based) that belongs to the group, header lines included. */
  lines: number[];
  rules: RobotsRule[];
}

/**
 * A small, deliberate robots.txt reader. It groups consecutive `User-agent`
 * lines the way the specification does, keeps the line number of every rule so
 * the file can be marked up where the reader is already looking, and does not
 * pretend to do more than that.
 */
function parseRobots(text: string): RobotsGroup[] {
  const groups: RobotsGroup[] = [];
  let current: RobotsGroup | null = null;
  let openedForAgents = false;

  text.split(/\r?\n/).forEach((raw, i) => {
    const line = raw.replace(/#.*$/, "").trim();
    if (!line) return;
    const at = line.indexOf(":");
    if (at < 0) return;
    const field = line.slice(0, at).trim().toLowerCase();
    const value = line.slice(at + 1).trim();

    if (field === "user-agent") {
      let g = current;
      if (!g || !openedForAgents) {
        g = { tokens: [], lines: [], rules: [] };
        groups.push(g);
        current = g;
        openedForAgents = true;
      }
      g.tokens.push(value.toLowerCase());
      g.lines.push(i);
      return;
    }
    const g = current;
    if (!g) return;
    openedForAgents = false;
    g.lines.push(i);
    if (field === "allow" || field === "disallow") {
      g.rules.push({ line: i, kind: field, path: value });
    }
  });

  return groups;
}

/** Longest matching product token wins, exactly as the crawlers resolve it. */
function groupFor(
  groups: RobotsGroup[],
  agent: { display_name: string; agent: string; robots_token: string },
): RobotsGroup | null {
  const names = [agent.display_name.toLowerCase(), agent.agent.toLowerCase(), agent.robots_token.toLowerCase()];
  let best: RobotsGroup | null = null;
  let bestLen = -1;
  for (const g of groups) {
    for (const t of g.tokens) {
      if (t === "*") continue;
      if (names.some((n) => n.includes(t) || t.includes(n))) {
        if (t.length > bestLen) {
          best = g;
          bestLen = t.length;
        }
      }
    }
  }
  if (best) return best;
  return groups.find((g) => g.tokens.includes("*")) ?? null;
}

/** The rule that decides whether the agent may read the site root at all. */
function rootRule(group: RobotsGroup | null): RobotsRule | null {
  if (!group) return null;
  const deny = group.rules.find((r) => r.kind === "disallow" && r.path === "/");
  if (deny) return deny;
  const allow = group.rules.find((r) => r.kind === "allow" && r.path === "/");
  if (allow) return allow;
  const openly = group.rules.find((r) => r.kind === "disallow" && r.path === "");
  return openly ?? null;
}

/* ── the file, rendered as the file ─────────────────────────────────────── */

function FileListing({
  doc,
  title,
  missing,
  agents,
  selected,
  onSelect,
}: {
  doc: PolicyDoc;
  title: string;
  missing: { title: string; body: string; action: string };
  agents?: PolicyAgentRow[];
  selected?: string | null;
  onSelect?: (id: string) => void;
}) {
  const lines = useMemo(() => (doc.text ?? "").split(/\r?\n/), [doc.text]);
  const groups = useMemo(() => (agents ? parseRobots(doc.text ?? "") : []), [doc.text, agents]);

  const agent = agents?.find((a) => a.agent === selected) ?? null;
  const group = agent ? groupFor(groups, agent) : null;
  const rule = rootRule(group);
  const groupLines = useMemo(() => new Set(group?.lines ?? []), [group]);

  if (!doc.present || !doc.text) {
    return (
      <Panel>
        <PanelHead title={title} note={doc.url} />
        <div className="mf-finding">
          <span className="mf-finding__mark">
            <IconAlert size={16} />
          </span>
          <div>
            <h3 className="mf-finding__title">{missing.title}</h3>
            <p className="mf-finding__body">{missing.body}</p>
            <p className="mf-finding__body mf-finding__do">
              <b>What to do.</b> {missing.action}
            </p>
          </div>
        </div>
      </Panel>
    );
  }

  return (
    <Panel>
      <PanelHead
        title={title}
        note={
          doc.fetched_at
            ? `${doc.url} · read ${ago(doc.fetched_at)}`
            : `${doc.url} · never read`
        }
      />
      {agents?.length ? (
        <>
          <div className="mf-file__pick">
            <span>Mark the rule that answers</span>
            <div className="mf-pivot" role="group" aria-label="Agent to mark in the file">
              {agents.slice(0, 6).map((a) => (
                <button
                  key={a.agent}
                  type="button"
                  className="mf-pivot__key"
                  aria-pressed={selected === a.agent}
                  onClick={() => onSelect?.(a.agent)}
                >
                  {agentName(a.display_name, a.agent)}
                </button>
              ))}
            </div>
          </div>
          {agent ? (
            <p className="mf-caveat mf-file__why">
              <IconInfo size={14} />
              <span>
                {group ? (
                  <>
                    <b>{agentName(agent.display_name, agent.agent)}</b> is answered by the group opening at line {(group.lines[0] ?? 0) + 1}
                    {group.tokens.includes("*") && !group.tokens.some((t) => t !== "*")
                      ? " (the catch-all group, because no rule names this agent)"
                      : ""}
                    . {rule
                      ? `Line ${rule.line + 1} decides its access to the site root.`
                      : `No rule in that group covers the site root, so it may read anything the group does not name.`}
                  </>
                ) : (
                  <>
                    No group in this file names <b>{agentName(agent.display_name, agent.agent)}</b>, and there is no catch-all group. Every
                    crawler reads that as permission.
                  </>
                )}
              </span>
            </p>
          ) : null}
        </>
      ) : null}
      <pre className="mf-file" tabIndex={0} aria-label={`${title} as served`}>
        {lines.map((text, i) => (
          <span
            className="mf-file__line"
            key={i}
            data-group={groupLines.has(i) ? "true" : undefined}
            data-rule={rule?.line === i ? "true" : undefined}
            data-comment={text.trim().startsWith("#") ? "true" : undefined}
          >
            <span className="mf-file__no" aria-hidden="true">
              {i + 1}
            </span>
            <span className="mf-file__txt">{text || " "}</span>
          </span>
        ))}
      </pre>
    </Panel>
  );
}

/* ── observed behaviour ─────────────────────────────────────────────────── */

/**
 * The catalog answers whole, and that is the right answer: an operator needs to
 * know the rule their own file gives an agent BEFORE it ever arrives. But it is
 * two readings under one heading, so it is drawn as two. The agents that
 * actually fetched lead the panel and are counted; the rest of the catalog
 * waits under a fold that says how many it holds.
 */
function splitAgents(agents: PolicyAgentRow[] | undefined): {
  observed: PolicyAgentRow[];
  catalog: PolicyAgentRow[];
} {
  const all = agents ?? [];
  return {
    observed: all.filter((a) => a.observed_fetches > 0),
    catalog: all.filter((a) => a.observed_fetches <= 0),
  };
}

function ObservedPanel({
  policy,
  onSelect,
}: {
  policy: ReturnType<typeof usePolicy>;
  onSelect: (id: string) => void;
}) {
  const robots: Column<PolicyAgentRow> = {
    key: "robots",
    head: "robots.txt",
    render: (r) => (
      <span className="mf-flag" data-tone={robotsState(r) === "allowed" ? "allow" : robotsState(r) === "disallowed" ? "deny" : "none"}>
        {robotsState(r) === "allowed" ? <IconCheck size={12} /> : robotsState(r) === "disallowed" ? <IconClose size={12} /> : null}
        {robotsState(r) === "allowed" ? "Allowed" : robotsState(r) === "disallowed" ? "Disallowed" : "Unlisted"}
      </span>
    ),
  };

  const llms: Column<PolicyAgentRow> = {
    key: "llms",
    head: "llms.txt",
    render: (r) => (
      <span className={r.listed_in_llms ? undefined : "mf-dim"}>{r.listed_in_llms ? "Listed" : "Not listed"}</span>
    ),
  };

  /** The agents that wrote on the paper. Every counting column earns its place. */
  const columns: Column<PolicyAgentRow>[] = [
    {
      key: "agent",
      head: "Agent",
      render: (r) => (
        <span className="mf-wedge-stack">
          <span>
            <VerifiedMark state={verifiedState(r)} /> {agentName(r.display_name, r.agent)}
          </span>
          <span className="mf-wedge-stack__sub">{titleCase(r.operator)}</span>
        </span>
      ),
    },
    robots,
    llms,
    {
      key: "fetches",
      head: "Fetches",
      numeric: true,
      render: (r) => <span className="mf-data">{num(r.observed_fetches)}</span>,
    },
    {
      key: "against",
      head: "Against rule",
      numeric: true,
      render: (r) =>
        r.violations > 0 ? (
          <span
            className="mf-flag mf-data"
            data-tone={r.verified_violations > 0 ? "deny" : "none"}
            title={
              r.verified_violations > 0
                ? `${num(r.verified_violations)} of ${num(r.violations)} fetches on disallowed paths came from an address that verified against the operator's published ranges.`
                : `${num(r.violations)} fetches on disallowed paths, none from an address that verified. Recorded, not attributed.`
            }
          >
            {num(r.violations)}
          </span>
        ) : (
          <span className="mf-data mf-dim">0</span>
        ),
    },
  ];

  /**
   * The agents that have not written. Fetches, violations and identity are all
   * zero or unknowable by definition, so those columns are dropped rather than
   * filled with noughts a reader has to discount. What is left is the useful
   * part: what this site's files already answer, and the token a group would
   * have to name to answer differently.
   */
  const ruleColumns: Column<PolicyAgentRow>[] = [
    {
      key: "agent",
      head: "Agent",
      render: (r) => (
        <span className="mf-wedge-stack">
          <span>{agentName(r.display_name, r.agent)}</span>
          <span className="mf-wedge-stack__sub">{titleCase(r.operator)}</span>
        </span>
      ),
    },
    robots,
    llms,
    {
      key: "token",
      head: "Names itself",
      render: (r) => (
        <span className="mf-path" title={`A User-agent group naming ${r.robots_token} answers this agent.`}>
          {r.robots_token}
        </span>
      ),
    },
  ];

  return (
    <Panel className="mf-wedge-tight">
      <PanelHead
        title="What they actually did"
        note="The agents that fetched this site in this interval, against the rule your own file gives each one. Selecting a row marks that agent's rule in the file beside this one. The rest of the catalog, which has not fetched, reads under the fold."
      />
      <Reading query={policy} waiting={<Waiting rows={6} />} isEmpty={() => false}>
        {(d) => {
          const { observed, catalog } = splitAgents(d.agents);
          return (
            <>
              {observed.length ? (
                <>
                  <p className="mf-wedge__caption">
                    {num(observed.length)} {observed.length === 1 ? "agent" : "agents"} fetched this site in
                    this interval.
                  </p>
                  <Sheet
                    rows={observed}
                    columns={columns}
                    getKey={(r) => r.agent}
                    onRowClick={(r) => onSelect(r.agent)}
                  />
                </>
              ) : (
                <Empty
                  title="No named agent has fetched this site in this interval"
                  body="Agent traffic is written by the server-side ingest path. Until it reports, there is nothing to compare this site's policy against. What the files already answer is still readable below."
                />
              )}
              {catalog.length ? (
                <div className="mf-wedge__body">
                  <Disclosure
                    summary={`${num(catalog.length)} catalogued agent${catalog.length === 1 ? " has" : "s have"} not fetched this site`}
                  >
                    <p className="mf-wedge__prose">
                      Micaforge holds these agents in its catalog, and none of them has fetched this site in
                      this interval. Your files answer them anyway, from the moment one arrives, so the rule
                      is listed here and the counts, which would all read zero, are not.
                    </p>
                    <Sheet
                      rows={catalog}
                      columns={ruleColumns}
                      getKey={(r) => r.agent}
                      onRowClick={(r) => onSelect(r.agent)}
                    />
                  </Disclosure>
                </div>
              ) : null}
            </>
          );
        }}
      </Reading>
      <div className="mf-wedge__body">
        <VerificationCaveat />
      </div>
    </Panel>
  );
}

/* ── violations ─────────────────────────────────────────────────────────── */

function ViolationsPanel({ bench }: { bench: MachineEnv }) {
  const q = useQuery({
    queryKey: bench.key("policy/violations"),
    // The endpoint answers { summary, rows }, not a bare array. Reading it as
    // an array made `.filter` throw and took the whole screen down with it, so
    // the shape is named here and normalised once.
    queryFn: async () =>
      (await api.get<{ summary?: ViolationsSummary; rows?: ViolationRow[] }>(
        "/api/policy/violations",
        bench.env,
      )).data,
    enabled: bench.ready,
    placeholderData: keepPreviousData,
  });

  const columns: Column<ViolationRow>[] = [
    {
      key: "agent",
      head: "Agent",
      render: (r) => (
        <span className="mf-wedge-stack">
          <span>{agentName(r.display_name, r.agent)}</span>
          <span className="mf-wedge-stack__sub">{titleCase(r.operator)}</span>
        </span>
      ),
    },
    {
      key: "path",
      head: "Path fetched",
      width: "34%",
      render: (r) => (
        <span className="mf-wedge-stack">
          <span className="mf-path" title={r.path}>
            {shortPath(r.path, 26)}
          </span>
          <span className="mf-wedge-stack__sub">last fetched {r.last_seen ? ago(r.last_seen) : "never"}</span>
        </span>
      ),
    },
    {
      key: "sources",
      head: "Sources",
      numeric: true,
      render: (r) => (
        <span
          className="mf-data"
          title={`${num(r.unique_sources)} distinct source addresses, held as rotating salted digests. Micaforge never stores an address.`}
        >
          {num(r.unique_sources)}
        </span>
      ),
    },
    { key: "n", head: "Fetches", numeric: true, render: (r) => <span className="mf-data">{num(r.fetches)}</span> },
  ];

  const rows: ViolationRow[] = Array.isArray(q.data?.rows) ? q.data.rows : [];
  // Only a verified identity may be reported as a violation. An unverified
  // claim is recorded and shown apart, never attributed to the operator whose
  // name the user-agent string borrowed.
  const confirmed = rows.filter((r) => r.compliance === "violation");
  const claimed = rows.filter((r) => r.compliance !== "violation");

  return (
    <Panel className="mf-wedge-tight">
      <PanelHead
        title="Fetched anyway"
        note="Paths an agent read that its own rule disallows. Only a verified identity can be reported as a violation."
      />
      <Reading
        query={q}
        waiting={<Waiting rows={4} />}
        isEmpty={() => false}
        empty={null}
      >
        {() =>
          confirmed.length === 0 ? (
            <Empty
              title="No verified agent broke a rule"
              body={
                claimed.length
                  ? "Nothing verified fetched a path it was refused. There are unverified claims below; they are recorded, not accused."
                  : "Every verified agent stayed inside the rules your robots.txt gives it."
              }
            />
          ) : (
            <Sheet rows={confirmed} columns={columns} getKey={(r) => `${r.agent}-${r.path}`} />
          )
        }
      </Reading>
      {claimed.length > 0 ? (
        <div className="mf-wedge__body">
          <Disclosure
            summary={`${num(claimed.length)} unverified claim${claimed.length === 1 ? "" : "s"}, recorded but not accused`}
          >
            <p className="mf-wedge__prose">
              These fetches carried the user-agent string of an agent that is disallowed here, but the source
              address did not match ranges the operator publishes, or the operator publishes none at all.
              Anyone can send any user-agent string, so Micaforge records these and stops short of calling
              them violations.
            </p>
            <Sheet rows={claimed} columns={columns} getKey={(r) => `${r.agent}-${r.path}`} />
          </Disclosure>
        </div>
      ) : null}
    </Panel>
  );
}

/* ── the refetch key ────────────────────────────────────────────────────── */

function usePolicy(bench: MachineEnv, polling: boolean) {
  return useQuery({
    queryKey: bench.key("policy"),
    queryFn: async () => (await api.get<PolicyResponse>("/api/policy", bench.env)).data,
    enabled: bench.ready,
    placeholderData: keepPreviousData,
    refetchInterval: polling ? 2000 : false,
  });
}

function RefetchKey({
  policy,
  job,
  setJob,
}: {
  policy: ReturnType<typeof usePolicy>;
  job: { startedAt: number; before: number | null; status?: string; progress?: number | null } | null;
  setJob: (j: { startedAt: number; before: number | null; status?: string; progress?: number | null } | null) => void;
}) {
  const [slow, setSlow] = useState(false);

  const run = useMutation({
    mutationFn: async () => (await api.post<RefetchJob>("/api/policy/refetch")).data,
    onSuccess: (d) => {
      setSlow(false);
      setJob({
        startedAt: Date.now(),
        before: policy.data?.robots.fetched_at ?? null,
        status: d?.status,
        progress: d?.progress,
      });
    },
  });

  /* The job is finished when the served file carries a newer read time than the
     one on screen when the key was pressed. No invented progress: if the server
     did not send a figure, none is drawn. */
  useEffect(() => {
    if (!job) return;
    const now = policy.data?.robots.fetched_at ?? null;
    if (now != null && (job.before == null || now > job.before)) {
      setJob(null);
      setSlow(false);
      return;
    }
    if (Date.now() - job.startedAt > 60_000) {
      setJob(null);
      setSlow(true);
    }
  }, [job, policy.data, setJob]);

  return (
    <div className="mf-job">
      <Button
        icon={<IconPolicy size={14} />}
        onClick={() => run.mutate()}
        disabled={run.isPending || job != null}
      >
        {job ? "Reading the files" : "Read both files again"}
      </Button>
      {job ? (
        <>
          <span>{job.status ? job.status : "queued"}</span>
          {typeof job.progress === "number" ? (
            <span className="mf-job__track" role="progressbar" aria-valuenow={Math.round(job.progress * 100)} aria-valuemin={0} aria-valuemax={100}>
              <span className="mf-job__pen" style={{ width: `${Math.max(0, Math.min(1, job.progress)) * 100}%` }} />
            </span>
          ) : null}
        </>
      ) : null}
      {slow ? <span>Still queued after a minute. This panel updates when it lands.</span> : null}
      {run.isError ? <span>{errorText(run.error)}</span> : null}
    </div>
  );
}

/* ── the screen ─────────────────────────────────────────────────────────── */

export function PolicyScreen() {
  const bench = useMachineEnv("agent");
  const [job, setJob] = useState<{ startedAt: number; before: number | null; status?: string; progress?: number | null } | null>(
    null,
  );
  const policy = usePolicy(bench, job != null);
  const [selected, setSelected] = useState<string | null>(null);

  /* The picker beside the file leads with the agents that actually fetched, so
     the keys it can show are the ones worth marking first. The rest of the
     catalog is still reachable, and still markable, from the sheet opposite. */
  const agents = useMemo(() => {
    const { observed, catalog } = splitAgents(policy.data?.agents);
    return [...observed, ...catalog];
  }, [policy.data]);
  const marked = selected ?? agents[0]?.agent ?? null;

  return (
    <div className="mf-wedge">
      <Bench
        title="Policy vs. observed"
        bench={bench}
        controls={<RefetchKey policy={policy} job={job} setJob={setJob} />}
      />

      <div className="mf-wedge-pair">
        <div className="mf-wedge-col">
          <Reading
            query={policy}
            waiting={
              <Panel>
                <Waiting rows={8} />
              </Panel>
            }
            isEmpty={() => false}
          >
            {(d) => (
              <>
                <FileListing
                  doc={d.robots}
                  title="robots.txt"
                  agents={agents}
                  selected={marked}
                  onSelect={setSelected}
                  missing={{
                    title: "This site serves no robots.txt",
                    body:
                      "Nothing at the domain root states which operators may read this site, so every crawler treats the whole site as open. That is a decision by default rather than a decision.",
                    action:
                      "Publish a robots.txt at the domain root with one User-agent group per operator you want to allow or refuse, then read the files again with the key above.",
                  }}
                />
                <FileListing
                  doc={d.llms}
                  title="llms.txt"
                  missing={{
                    title: "This site serves no llms.txt",
                    body:
                      "Nothing tells an answer engine which pages are the canonical ones to read, so it answers from whichever copy it already holds, often an old one, often somebody else's.",
                    action:
                      "Publish /llms.txt listing your documentation entry points as markdown links, shortest path first, then read the files again.",
                  }}
                />
              </>
            )}
          </Reading>
        </div>

        <div className="mf-wedge-col">
          <ObservedPanel policy={policy} onSelect={setSelected} />
          <ViolationsPanel bench={bench} />
          <Panel>
            <div className="mf-finding" data-tone="note">
              <span className="mf-finding__mark">
                <IconInfo size={16} />
              </span>
              <div>
                <h3 className="mf-finding__title">How verification works here</h3>
                <p className="mf-finding__body">
                  Micaforge resolves the source address of every agent fetch against the address ranges and
                  reverse-DNS suffixes the operator publishes, using the catalog in{" "}
                  <span className="mf-path">agent_catalog</span>. A match makes the identity verified. No
                  published ranges, or an address outside them, leaves it unverified, and an unverified
                  fetch is never counted as a violation, however plainly its user-agent claims a name.
                </p>
                <p className="mf-finding__body mf-finding__do">
                  <a className="mf-ext" href="/docs" target="_blank" rel="noreferrer noopener">
                    Read how the catalog is built and kept current
                    <IconOut size={12} />
                  </a>
                </p>
              </div>
            </div>
          </Panel>
        </div>
      </div>
    </div>
  );
}

export default PolicyScreen;
