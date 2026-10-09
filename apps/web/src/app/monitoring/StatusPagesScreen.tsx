/**
 * STATUS PAGES, at /app/:site/status-pages
 *
 * The one question: what do this site's customers see when they ask "is it
 * down, or is it me?"
 *
 * A status page is a public page at /status/<slug> that shows the monitors the
 * owner chose (under public names, never their URLs), any open incident or
 * maintenance, and ninety days of history. This screen lists the site's pages,
 * makes and changes them, and, for the selected page, posts the incidents and
 * maintenance notices its readers see.
 *
 * Selection lives in the address (`?page=`), so a teammate can be sent a link
 * straight to the page that needs an update during an outage.
 *
 * Roles split the work the way the API does: changing a page's settings needs
 * the admin role (it publishes data), posting an incident needs member. The
 * screen offers both actions to everyone and prints the server's refusal when
 * the role falls short, because the dashboard is not told the reader's role
 * and a hidden button explains nothing.
 */
import { useState } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useSearchParams } from "react-router";
import { Button, CopyField, Empty, ErrorState, Panel, PanelHead, Sheet } from "../../components/react/ui";
import type { Column } from "../../components/react/ui";
import { IconOut, IconPen, IconPlus } from "../../components/react/icons";
import { when } from "../../components/status/words";
import { api } from "../../lib/api";
import { num } from "../../lib/format";
import { useSite } from "../providers";
import { ConfirmRow, Say } from "./parts";
import { StatusIncidents } from "./StatusIncidents";
import { StatusPageEditor } from "./StatusPageEditor";
import type { StatusPage } from "./types";
import { keys, problem } from "./types";

function PublishedMark({ published }: { published: boolean }) {
  return (
    <span className="mf-mon-pub" data-published={published}>
      {published ? "Published" : "Not published"}
    </span>
  );
}

const COLUMNS = (selected: string | undefined): Column<StatusPage>[] => [
  {
    key: "title",
    head: "Title",
    render: (p) => (
      <span className="mf-mon-name" data-selected={p.id === selected ? "true" : undefined}>
        {p.title}
        {p.id === selected ? <span className="mf-sr"> (shown below)</span> : null}
      </span>
    ),
  },
  {
    key: "slug",
    head: "Address",
    render: (p) => <span className="mf-data mf-mon-url">/status/{p.slug}</span>,
  },
  { key: "published", head: "Visibility", render: (p) => <PublishedMark published={p.published} /> },
  {
    key: "monitors",
    head: "Monitors",
    numeric: true,
    render: (p) => <span className="mf-data">{num(p.monitors.length)}</span>,
  },
  { key: "created", head: "Created", render: (p) => <span className="mf-data">{when(p.created_at)}</span> },
];

/* ── one page: its address and settings at a glance ──────────────────────── */

function PageSummary({
  siteId,
  page,
  onEdit,
  onDeleted,
}: {
  siteId: number;
  page: StatusPage;
  onEdit: () => void;
  onDeleted: () => void;
}) {
  const qc = useQueryClient();
  const [confirming, setConfirming] = useState(false);

  const remove = useMutation({
    mutationFn: () => api.del<{ deleted: string }>(`/api/status-pages/${page.id}`),
    onSuccess: async () => {
      await qc.invalidateQueries({ queryKey: keys.pages(siteId) });
      onDeleted();
    },
  });

  const shown = [...page.monitors].sort((a, b) => a.position - b.position);

  return (
    <Panel className="mf-mon-detail">
      <PanelHead
        title={page.title}
        note={
          page.published
            ? "Published. Anyone with the address can read this page."
            : "Not published. The address says there is no status page there until you publish it."
        }
        actions={
          <>
            <Button size="sm" icon={<IconPen size={13} />} onClick={onEdit}>
              Edit
            </Button>
            <Button size="sm" tone="danger" disabled={confirming} onClick={() => setConfirming(true)}>
              Delete
            </Button>
          </>
        }
      />

      {confirming ? (
        <ConfirmRow
          confirm="Delete status page"
          busy={remove.isPending}
          onConfirm={() => remove.mutate()}
          onCancel={() => {
            setConfirming(false);
            remove.reset();
          }}
        >
          Delete <strong>{page.title}</strong>? Its address stops working and its incidents and maintenance notices go
          with it. The monitors themselves are not changed. This cannot be undone.
          {remove.isError ? (
            <>
              {" "}
              <Say tone="warn">{problem(remove.error)}</Say>
            </>
          ) : null}
        </ConfirmRow>
      ) : null}

      <div className="mf-mon-band mf-mon-address">
        <CopyField label="Public address" value={page.public_url} />
        <a className="mf-btn mf-btn--key mf-btn--md mf-mon-open" href={page.public_url} target="_blank" rel="noopener">
          <span className="mf-btn__icon">
            <IconOut size={14} />
          </span>
          <span className="mf-btn__label">Open</span>
          <span className="mf-sr"> (opens in a new tab)</span>
        </a>
      </div>

      <dl className="mf-mon-facts">
        <div>
          <dt>Monitors shown</dt>
          <dd>
            {shown.length
              ? shown.map((m) => (m.display_name ? `${m.display_name} (${m.name})` : m.name)).join(", ")
              : "None yet. Edit the page to choose which monitors it shows."}
          </dd>
        </div>
        {page.description ? (
          <div>
            <dt>Description</dt>
            <dd>{page.description}</dd>
          </div>
        ) : null}
        <div>
          <dt>Accent colour</dt>
          <dd>
            <span className="mf-mon-swatch" style={{ background: page.accent_color }} aria-hidden="true" />
            <span className="mf-data">{page.accent_color}</span>
          </dd>
        </div>
        {page.footer_label && page.footer_url ? (
          <div>
            <dt>Footer link</dt>
            <dd>
              {page.footer_label} <span className="mf-data mf-mon-url">{page.footer_url}</span>
            </dd>
          </div>
        ) : null}
      </dl>
    </Panel>
  );
}

/* ── the screen ──────────────────────────────────────────────────────────── */

export function StatusPagesScreen() {
  const { siteId, domain } = useSite();
  const [params, setParams] = useSearchParams();
  /** "new" while making a page, a page id while editing one. */
  const [editing, setEditing] = useState<string | null>(null);

  const pages = useQuery({
    queryKey: keys.pages(siteId),
    queryFn: () => api.get<StatusPage[]>("/api/status-pages", { site: siteId as number }).then((r) => r.data),
    enabled: siteId != null,
  });

  const rows = pages.data ?? [];
  const wanted = params.get("page");
  const current = rows.find((p) => p.id === wanted) ?? rows[0];

  const select = (id: string | null) =>
    setParams(
      (prev) => {
        const next = new URLSearchParams(prev);
        if (id) next.set("page", id);
        else next.delete("page");
        return next;
      },
      { replace: true },
    );

  const finish = (saved?: StatusPage) => {
    setEditing(null);
    if (saved) select(saved.id);
  };

  const none = pages.isSuccess && rows.length === 0;
  const newButton = (
    <Button tone="primary" icon={<IconPlus size={14} />} disabled={editing === "new"} onClick={() => setEditing("new")}>
      New status page
    </Button>
  );

  return (
    <div className="mf-mon">
      <h1 className="mf-sr">Status pages</h1>

      {editing === "new" && siteId != null ? <StatusPageEditor siteId={siteId} domain={domain} onDone={finish} /> : null}

      <Panel>
        <PanelHead
          title="Status pages"
          note="A public page that shows the state of the monitors you choose, with the incidents and maintenance you post. Readers need no account."
          actions={none ? undefined : newButton}
        />
        {pages.isError ? (
          <ErrorState title="Could not load the status pages" detail={problem(pages.error)} onRetry={() => void pages.refetch()} />
        ) : none ? (
          <Empty
            title="No status pages yet"
            body="A status page tells this site's customers whether it is working, at an address you can link from your own site and emails. Make one, choose its monitors, then publish it."
            action={editing === "new" ? undefined : newButton}
          />
        ) : (
          <div className="mf-mon-list">
            <Sheet
              rows={rows}
              columns={COLUMNS(current?.id)}
              getKey={(p) => p.id}
              loading={pages.isPending}
              onRowClick={(p) => {
                select(p.id);
                if (editing && editing !== "new") setEditing(null);
              }}
            />
          </div>
        )}
      </Panel>

      {current && siteId != null ? (
        <>
          {editing === current.id ? (
            <StatusPageEditor key={current.id} siteId={siteId} domain={domain} page={current} onDone={finish} />
          ) : (
            <PageSummary
              key={current.id}
              siteId={siteId}
              page={current}
              onEdit={() => setEditing(current.id)}
              onDeleted={() => select(null)}
            />
          )}
          <StatusIncidents key={`incidents-${current.id}`} page={current} />
        </>
      ) : null}
    </div>
  );
}

export default StatusPagesScreen;
