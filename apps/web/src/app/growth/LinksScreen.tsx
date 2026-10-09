/**
 * Short links, at /app/:site/links
 *
 * A short link is a short address that redirects to a longer one. Micaforge
 * counts each click on the server, so the count needs no cookie and no script
 * on the target page, and it answers chat-app preview bots with a card of the
 * operator's choosing. This screen is where those links are made, sorted and
 * read.
 *
 * Layout, and why:
 *
 *   - THE LIST comes first and keeps its place. Opening a link does not push
 *     the list down or replace it: on a wide screen the link opens in a column
 *     beside the list, and on a narrow one it opens under it and takes focus.
 *     The opened link is in the address (`?link=<id>`), so it survives a
 *     reload and can be sent to a colleague.
 *   - THE EDITOR opens in that same column, so a new link is written next to
 *     the list it will join.
 *   - THE CLICKS for the opened link run the full width under both, because a
 *     chart and five ranked lists need the room.
 *
 * Two scopes: "This site" lists the site's links; "Organisation" lists the
 * links that belong to the whole organisation and no single site. The
 * organisation id comes from the site record; when it is missing, only the
 * site scope is offered.
 */
import { useEffect, useMemo, useState } from "react";
import { keepPreviousData, useQuery } from "@tanstack/react-query";
import { useSearchParams } from "react-router";

import { Button, Empty, ErrorState, Panel, PanelHead, Sheet } from "../../components/react/ui";
import type { Column } from "../../components/react/ui";
import { IconPlus, IconSearch } from "../../components/react/icons";
import { api } from "../../lib/api";
import { hostOf, num } from "../../lib/format";
import { useSite } from "../providers";
import { Bank, Choice, CopyKey, TextInput, WaitLines, useDebounced } from "./controls";
import { FolderManager } from "./FolderManager";
import { LinkDetail } from "./LinkDetail";
import { LinkEditor } from "./LinkEditor";
import { LinkStats } from "./LinkStats";
import { bareUrl, day, orgOf, problem, scopeKey, scopeQuery } from "./model";
import type { Folder, Link, LinkScope, LinksConfig } from "./model";

type Editing = null | { kind: "new" } | { kind: "edit"; link: Link };

function useLinksConfig() {
  return useQuery({
    queryKey: ["links", "config"],
    queryFn: () => api.get<LinksConfig>("/api/links/config").then((r) => r.data),
    staleTime: 10 * 60_000,
  });
}

/** What a link explains about itself in the empty state and the panel note. */
function ServedAt({ config }: { config: LinksConfig | undefined }) {
  if (!config) return null;
  const base = config.short_base.replace(/\/+$/, "");
  return (
    <>
      {" "}
      Short links are served at <span className="mf-data">{base}/</span>
      {config.links_host ? null : (
        <>
          {" "}
          because no separate short-link host is set (<span className="mf-data">MICAFORGE_LINKS_HOST</span>)
        </>
      )}
      .
    </>
  );
}

export function LinksScreen() {
  const { site, siteId, isLoading: siteLoading } = useSite();
  const org = orgOf(site);
  const [params, setParams] = useSearchParams();
  const openId = params.get("link");

  const [scopeKind, setScopeKind] = useState<"site" | "org">("site");
  const [folder, setFolder] = useState<string>("all");
  const [q, setQ] = useState("");
  const query = useDebounced(q.trim(), 250);
  const [editing, setEditing] = useState<Editing>(null);
  const [foldersOpen, setFoldersOpen] = useState(false);

  const scope: LinkScope | null =
    scopeKind === "org" && org ? { kind: "org", org } : siteId != null ? { kind: "site", site: siteId } : null;
  const key = scopeKey(scope);

  const config = useLinksConfig();

  const folders = useQuery({
    queryKey: ["links", "folders", key],
    queryFn: () => api.get<Folder[]>(`/api/link-folders?${scopeQuery(scope as LinkScope)}`).then((r) => r.data),
    enabled: scope != null,
  });
  const folderList = useMemo(
    () => [...(folders.data ?? [])].sort((a, b) => a.name.localeCompare(b.name)),
    [folders.data],
  );

  const links = useQuery({
    queryKey: ["links", "list", key, folder, query],
    queryFn: async () => {
      const p = new URLSearchParams(scopeQuery(scope as LinkScope));
      if (folder !== "all") p.set("folder", folder);
      if (query) p.set("q", query);
      return (await api.get<Link[]>(`/api/links?${p.toString()}`)).data;
    },
    enabled: scope != null,
    placeholderData: keepPreviousData,
  });
  const rows = links.data ?? [];

  const fromList = openId ? rows.find((l) => l.id === openId) : undefined;
  const opened = useQuery({
    queryKey: ["links", "one", openId],
    queryFn: () => api.get<Link>(`/api/links/${encodeURIComponent(openId as string)}`).then((r) => r.data),
    enabled: openId != null,
    placeholderData: fromList,
  });
  const openLink = opened.data ?? fromList ?? null;

  // A folder that was deleted while it was the filter leaves nothing to show.
  useEffect(() => {
    if (folder === "all" || folder === "none" || !folders.data) return;
    if (!folders.data.some((f) => f.id === folder)) setFolder("all");
  }, [folder, folders.data]);

  const open = (id: string | null) => {
    setParams(
      (prev) => {
        const next = new URLSearchParams(prev);
        if (id) next.set("link", id);
        else next.delete("link");
        return next;
      },
      { replace: true },
    );
  };

  const changeScope = (v: "site" | "org") => {
    setScopeKind(v);
    setEditing(null);
    open(null);
  };

  const filtered = folder !== "all" || query.length > 0;
  const folderName = (id: string | null) =>
    id ? (folderList.find((f) => f.id === id)?.name ?? "—") : "No folder";

  const columns: Column<Link>[] = [
    {
      key: "short",
      head: "Short link",
      render: (l) => (
        <span className="mf-gr-cell" data-open={l.id === openId ? "true" : "false"}>
          <span className="mf-gr-cell__slug mf-data" title={l.short_url}>
            {bareUrl(l.short_url)}
          </span>
          <CopyKey value={l.short_url} label={`Copy ${bareUrl(l.short_url)}`} />
          {l.id === openId ? <span className="mf-sr">(open)</span> : null}
        </span>
      ),
    },
    {
      key: "to",
      head: "Goes to",
      render: (l) => (
        <span className="mf-gr-cell__to" title={l.target_url}>
          {l.title?.trim() || hostOf(l.target_url)}
        </span>
      ),
    },
    { key: "folder", head: "Folder", render: (l) => <span>{folderName(l.folder_id)}</span> },
    {
      key: "clicks",
      head: "Clicks, 30 days",
      numeric: true,
      render: (l) => <span className="mf-data">{num(l.clicks_30d)}</span>,
    },
    {
      key: "expires",
      head: "Expires",
      render: (l) =>
        l.expired ? (
          <span className="mf-gr-state" data-tone="off">
            Expired
          </span>
        ) : (
          <span>{l.expires_at ? day(l.expires_at) : "Never"}</span>
        ),
    },
    { key: "created", head: "Created", render: (l) => <span>{day(l.created_at)}</span> },
  ];

  const newButton = (
    <Button
      tone="primary"
      icon={<IconPlus size={13} />}
      disabled={scope == null}
      onClick={() => setEditing({ kind: "new" })}
    >
      New link
    </Button>
  );

  const side = editing != null || openId != null;
  const showStats = openLink != null && (editing == null || editing.kind === "edit");

  return (
    <div className="mf-gr">
      <h1 className="mf-sr">Short links</h1>

      <div className="mf-gr-links" data-split={side ? "true" : "false"}>
        <Panel className="mf-gr-links__list">
          <PanelHead
            title="Short links"
            note={
              <>
                Each short link redirects to its target and counts the click without cookies.
                <ServedAt config={config.data} />
              </>
            }
            actions={newButton}
          />

          <div className="mf-gr-strip">
            <span className="mf-gr-search">
              <IconSearch size={14} className="mf-gr-search__icon" />
              <TextInput
                type="search"
                aria-label="Search links by slug, title or target"
                placeholder="Search links"
                value={q}
                onChange={(e) => setQ(e.target.value)}
              />
            </span>
            <Choice aria-label="Folder" value={folder} onChange={(e) => setFolder(e.target.value)}>
              <option value="all">All folders</option>
              <option value="none">No folder</option>
              {folderList.map((f) => (
                <option key={f.id} value={f.id}>
                  {f.name}
                </option>
              ))}
            </Choice>
            {org ? (
              <Bank<"site" | "org">
                label="Whose links"
                value={scopeKind}
                options={[
                  { v: "site", label: "This site" },
                  { v: "org", label: "Organisation" },
                ]}
                onChange={changeScope}
              />
            ) : null}
            <Button
              size="sm"
              tone="quiet"
              aria-expanded={foldersOpen}
              onClick={() => setFoldersOpen((v) => !v)}
            >
              {foldersOpen ? "Hide folders" : "Manage folders"}
            </Button>
          </div>

          {scopeKind === "org" && org ? (
            <p className="mf-gr-aside">
              Organisation links belong to no single site. Every site in the organisation can use them.
            </p>
          ) : null}

          {foldersOpen && scope ? (
            <FolderManager
              scope={scope}
              folders={folderList}
              loading={folders.isPending}
              error={folders.isError ? folders.error : null}
              onRetry={() => void folders.refetch()}
              onDeleted={(id) => {
                if (folder === id) setFolder("all");
              }}
            />
          ) : null}

          {links.isError ? (
            <ErrorState
              title="Could not load the short links"
              detail={problem(links.error)}
              onRetry={() => void links.refetch()}
            />
          ) : (
            <Sheet<Link>
              rows={rows}
              columns={columns}
              loading={siteLoading || scope == null || links.isPending}
              getKey={(l) => l.id}
              onRowClick={(l) => {
                setEditing(null);
                open(l.id);
              }}
              empty={
                filtered ? (
                  <Empty
                    title="No links match"
                    body="No link in this view matches the search and folder you chose. Clear them to see every link."
                    action={
                      <Button
                        onClick={() => {
                          setQ("");
                          setFolder("all");
                        }}
                      >
                        Clear search and folder
                      </Button>
                    }
                  />
                ) : (
                  <Empty
                    title={scopeKind === "org" ? "No organisation links yet" : "No short links yet"}
                    body={
                      <>
                        A short link redirects from a short address to your target address. Micaforge counts
                        each click without cookies, and shows your preview card when the link is pasted into a
                        chat app.
                        <ServedAt config={config.data} />
                      </>
                    }
                    action={newButton}
                  />
                )
              }
            />
          )}
        </Panel>

        {side ? (
          <div className="mf-gr-links__side">
            {editing && scope ? (
              <LinkEditor
                key={editing.kind === "edit" ? editing.link.id : "new"}
                link={editing.kind === "edit" ? editing.link : null}
                scope={scope}
                folders={folderList}
                config={config.data}
                onCancel={() => setEditing(null)}
                onSaved={(saved) => {
                  setEditing(null);
                  open(saved.id);
                }}
              />
            ) : openLink ? (
              <LinkDetail
                link={openLink}
                folders={folderList}
                onEdit={() => setEditing({ kind: "edit", link: openLink })}
                onClose={() => open(null)}
                onDeleted={() => open(null)}
              />
            ) : opened.isError ? (
              <Panel>
                <ErrorState
                  title="Could not open this link"
                  detail={problem(opened.error)}
                  onRetry={() => void opened.refetch()}
                />
                <div className="mf-gr-pad mf-gr-center">
                  <Button onClick={() => open(null)}>Close</Button>
                </div>
              </Panel>
            ) : (
              <Panel>
                <div className="mf-gr-pad">
                  <WaitLines lines={4} label="Opening the link…" />
                </div>
              </Panel>
            )}
          </div>
        ) : null}

        {showStats && openLink ? (
          <div className="mf-gr-links__stats">
            <LinkStats link={openLink} />
          </div>
        ) : null}
      </div>
    </div>
  );
}

export default LinksScreen;
