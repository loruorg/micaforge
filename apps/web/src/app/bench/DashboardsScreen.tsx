/**
 * DASHBOARDS: the index of user-built dashboards.
 *
 * A dashboard is a set of cards a user keeps together. The index shows each
 * one as a PLAN: the real card rectangles from the stored layout, drawn at
 * scale. It is not a screenshot and not a placeholder; it is the same geometry
 * the editor arranges, so the row tells you which dashboard this is before you
 * open it.
 *
 * This file also owns the dashboard MODEL (`DashLayout`, `DashCard`) and the
 * `/api/dashboards` calls, because the index is the resource's home. The editor
 * borrows both.
 */
import { useState } from "react";
import { Link, useParams } from "react-router";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { api } from "../../lib/api";
import type { Audience, Filter, Granularity } from "../../lib/api";
import { Button, Empty, Panel, PanelHead } from "../../components/react/ui";
import { IconClose, IconCopy, IconPlus } from "../../components/react/icons";
import { ago } from "../../lib/format";
import { useSite } from "../providers";
import { Reading, TextInput, dimensionLabel, metricDef } from "./QueryScreen";
import type { MetricKey } from "./QueryScreen";

/* ═══ the dashboard model ═════════════════════════════════════════════════ */

/**
 * How one card is drawn. Four, and no more, because a fifth would be noise.
 * The stored keys are older than the labels: "record" is the line chart and
 * "sheet" is the table. They stay as they are because saved layouts use them.
 */
export type CardVis = "record" | "sheet" | "figure" | "distribution";

export const VISUALS: { v: CardVis; label: string; note: string }[] = [
  { v: "figure", label: "Number", note: "One number for the whole period." },
  { v: "record", label: "Line chart", note: "The metric over time." },
  { v: "sheet", label: "Table", note: "Ranked rows with their metrics." },
  { v: "distribution", label: "Distribution", note: "Ranked shares of one dimension." },
];

export interface DashCard {
  id: string;
  title: string;
  vis: CardVis;
  metric: MetricKey;
  /** Required by `sheet` and `distribution`; ignored by the other two. */
  dimension: string;
  /** Overrides the screen's audience for this one card. */
  audience: Audience | "inherit";
  /** Added to the screen's own conditions, never instead of them. */
  filters: Filter[];
  limit: number;
  granularity: Granularity | "";
  x: number;
  y: number;
  w: number;
  h: number;
}

export interface DashLayout {
  v: 1;
  cols: number;
  cards: DashCard[];
}

export interface Dashboard {
  id: string;
  site_id: number;
  name: string;
  layout: DashLayout;
  updated_at?: string;
  created_at?: string;
}

/** The arrangement grid: twelve columns. */
export const GRID_COLS = 12;

export const emptyLayout = (): DashLayout => ({ v: 1, cols: GRID_COLS, cards: [] });

export function newCardId(): string {
  return `c${Date.now().toString(36)}${Math.random().toString(36).slice(2, 6)}`;
}

export function newCard(partial?: Partial<DashCard>): DashCard {
  return {
    id: newCardId(),
    title: "",
    vis: "figure",
    metric: "visitors",
    dimension: "pathname",
    audience: "inherit",
    filters: [],
    limit: 8,
    granularity: "",
    x: 0,
    y: 0,
    w: 3,
    h: 2,
    ...partial,
  };
}

/** A card's own name when the user did not give it one. */
export function cardTitle(c: DashCard): string {
  if (c.title.trim()) return c.title.trim();
  const m = metricDef(c.metric).label;
  if (c.vis === "sheet" || c.vis === "distribution") return `${m} by ${dimensionLabel(c.dimension).toLowerCase()}`;
  if (c.vis === "record") return `${m} over time`;
  return m;
}

/**
 * Layouts come back from a jsonb column, so they are treated as untrusted:
 * anything missing gets a sane value rather than crashing the editor.
 */
export function normaliseLayout(raw: unknown): DashLayout {
  let src: unknown = raw;
  if (typeof src === "string") {
    try {
      src = JSON.parse(src);
    } catch {
      src = null;
    }
  }
  const o = (src ?? {}) as Partial<DashLayout>;
  const cols = Number.isFinite(o.cols) && (o.cols as number) > 0 ? Math.round(o.cols as number) : GRID_COLS;
  const cards = Array.isArray(o.cards) ? o.cards : [];
  return {
    v: 1,
    cols,
    cards: cards.map((c, i) => {
      const card = (c ?? {}) as Partial<DashCard>;
      const w = clampInt(card.w, 2, cols, 3);
      return {
        ...newCard(),
        ...card,
        id: card.id || newCardId(),
        w,
        h: clampInt(card.h, 2, 12, 2),
        x: clampInt(card.x, 0, Math.max(0, cols - w), 0),
        y: clampInt(card.y, 0, 400, i),
        filters: Array.isArray(card.filters) ? card.filters : [],
      } as DashCard;
    }),
  };
}

function clampInt(v: unknown, min: number, max: number, fallback: number): number {
  const n = Math.round(Number(v));
  if (!Number.isFinite(n)) return fallback;
  return Math.max(min, Math.min(max, n));
}

export const dashboardsKey = (siteId: number | undefined) => ["bench", "dashboards", siteId];

export async function fetchDashboards(siteId: number): Promise<Dashboard[]> {
  const res = await api.get<Dashboard[]>("/api/dashboards", { site: siteId });
  return (res.data ?? []).map((d) => ({ ...d, layout: normaliseLayout(d.layout) }));
}

/* ═══ the plan ════════════════════════════════════════════════════════════ */

/** The dashboard drawn at scale from its own layout. */
function Plan({ layout }: { layout: DashLayout }) {
  const rows = Math.max(3, ...layout.cards.map((c) => c.y + c.h));
  const cols = layout.cols;
  const unit = 6;
  const pad = 3;
  const w = cols * unit + pad * 2;
  const h = rows * unit + pad * 2;
  return (
    <svg
      className="mf-bn-plan"
      viewBox={`0 0 ${w} ${h}`}
      preserveAspectRatio="xMidYMid meet"
      aria-hidden="true"
    >
      {layout.cards.map((c) => (
        <rect
          key={c.id}
          x={pad + c.x * unit + 0.5}
          y={pad + c.y * unit + 0.5}
          width={Math.max(1, c.w * unit - 1)}
          height={Math.max(1, c.h * unit - 1)}
          rx="0.8"
          fill="var(--mf-blue-wash)"
          stroke="var(--mf-s3)"
          strokeWidth="0.6"
        />
      ))}
    </svg>
  );
}

/* ═══ the index ═══════════════════════════════════════════════════════════ */

export function DashboardsScreen() {
  const { site } = useParams();
  const { siteId, isLoading: siteLoading } = useSite();
  const qc = useQueryClient();

  const [composing, setComposing] = useState(false);
  const [name, setName] = useState("");
  const [confirming, setConfirming] = useState<string | null>(null);

  const q = useQuery({
    queryKey: dashboardsKey(siteId),
    enabled: siteId != null,
    queryFn: () => fetchDashboards(siteId as number),
  });

  const create = useMutation({
    mutationFn: (input: { name: string; layout: DashLayout }) =>
      api.post<Dashboard>("/api/dashboards", {
        site_id: siteId,
        name: input.name,
        layout: input.layout,
      }),
    onSuccess: () => {
      setComposing(false);
      setName("");
      void qc.invalidateQueries({ queryKey: dashboardsKey(siteId) });
    },
  });

  const remove = useMutation({
    mutationFn: (id: string) => api.del<void>(`/api/dashboards/${id}`),
    onSuccess: () => {
      setConfirming(null);
      void qc.invalidateQueries({ queryKey: dashboardsKey(siteId) });
    },
  });

  const boards = q.data ?? [];

  return (
    <div className="mf-bn-page">
      <Panel>
        <PanelHead
          title="Dashboards"
          note="A dashboard is a set of charts and tables you keep together. Each card has its own metric, dimension and conditions."
          actions={
            composing ? null : (
              <Button tone="primary" icon={<IconPlus size={13} />} onClick={() => setComposing(true)}>
                New dashboard
              </Button>
            )
          }
        />

        {composing ? (
          <div className="mf-bn-compose">
            <label className="mf-bn-compose__label" htmlFor="mf-new-sheet">
              Name this dashboard
            </label>
            <TextInput
              id="mf-new-sheet"
              value={name}
              onChange={setName}
              placeholder="Docs and the AI crawlers reading them"
              autoFocus
              onKeyDown={(e) => {
                if (e.key === "Enter" && name.trim()) {
                  e.preventDefault();
                  create.mutate({ name: name.trim(), layout: emptyLayout() });
                }
                if (e.key === "Escape") setComposing(false);
              }}
            />
            <Button
              tone="primary"
              disabled={!name.trim() || create.isPending}
              onClick={() => create.mutate({ name: name.trim(), layout: emptyLayout() })}
            >
              {create.isPending ? "Creating…" : "Create"}
            </Button>
            <Button
              tone="quiet"
              onClick={() => {
                setComposing(false);
                setName("");
              }}
            >
              Cancel
            </Button>
            {create.isError ? (
              <p className="mf-bn-field__err" role="alert">
                {(create.error as Error)?.message ?? "The dashboard was not created."}
              </p>
            ) : null}
          </div>
        ) : null}

        <div className="mf-bn-indexwrap">
          <Reading
            loading={siteLoading || q.isLoading}
            error={q.isError ? q.error : undefined}
            onRetry={() => void q.refetch()}
            isEmpty={boards.length === 0}
            lines={3}
            empty={
              <Empty
                title="No dashboards yet"
                body="A dashboard holds the numbers you check together: a metric, a dimension, how to show it, and its own conditions. Arrange the cards once and the dashboard keeps that layout."
                action={
                  <Button tone="primary" icon={<IconPlus size={13} />} onClick={() => setComposing(true)}>
                    New dashboard
                  </Button>
                }
              />
            }
          >
            <ul className="mf-bn-index">
              {boards.map((d) => {
                const titles = d.layout.cards.map(cardTitle);
                return (
                  <li key={d.id} className="mf-bn-index__row">
                    <Link className="mf-bn-index__plan" to={`/app/${site}/dashboards/${d.id}`} tabIndex={-1} aria-hidden="true">
                      <Plan layout={d.layout} />
                    </Link>

                    <div className="mf-bn-index__text">
                      <Link className="mf-bn-index__name" to={`/app/${site}/dashboards/${d.id}`}>
                        {d.name || "Untitled dashboard"}
                      </Link>
                      <p className="mf-bn-index__meta">
                        <span className="mf-data">{d.layout.cards.length}</span>{" "}
                        {d.layout.cards.length === 1 ? "card" : "cards"}
                        {d.updated_at ? <> · changed {ago(d.updated_at)}</> : null}
                      </p>
                      {titles.length ? (
                        <p className="mf-bn-index__cards">
                          {titles.slice(0, 3).join(" · ")}
                          {titles.length > 3 ? ` · and ${titles.length - 3} more` : ""}
                        </p>
                      ) : (
                        <p className="mf-bn-index__cards">Empty. Open it to add the first card.</p>
                      )}
                    </div>

                    <div className="mf-bn-index__act">
                      {confirming === d.id ? (
                        <>
                          <span className="mf-bn-index__ask">Delete this dashboard?</span>
                          <Button
                            tone="danger"
                            size="sm"
                            disabled={remove.isPending}
                            onClick={() => remove.mutate(d.id)}
                          >
                            Delete
                          </Button>
                          <Button size="sm" tone="quiet" onClick={() => setConfirming(null)}>
                            Keep
                          </Button>
                        </>
                      ) : (
                        <>
                          <Button
                            size="sm"
                            icon={<IconCopy size={13} />}
                            disabled={create.isPending}
                            onClick={() =>
                              create.mutate({
                                name: `${d.name} (copy)`,
                                layout: {
                                  ...d.layout,
                                  cards: d.layout.cards.map((c) => ({ ...c, id: newCardId() })),
                                },
                              })
                            }
                          >
                            Duplicate
                          </Button>
                          <Button
                            size="sm"
                            tone="quiet"
                            icon={<IconClose size={13} />}
                            aria-label={`Delete ${d.name}`}
                            onClick={() => setConfirming(d.id)}
                          />
                        </>
                      )}
                    </div>
                  </li>
                );
              })}
            </ul>
          </Reading>

          {remove.isError ? (
            <p className="mf-bn-field__err" role="alert">
              That dashboard was not deleted. {(remove.error as Error)?.message}
            </p>
          ) : null}
        </div>
      </Panel>
    </div>
  );
}

export default DashboardsScreen;
