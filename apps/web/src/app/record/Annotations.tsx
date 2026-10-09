/**
 * Annotations a person writes on the traffic chart: "Launched v2", "Docs moved
 * to the new domain", "Outage, 40 minutes".
 *
 * A chart says that traffic changed; only the people who run the site know
 * why. These notes are that knowledge, kept on the server so everyone who reads
 * the dashboard (and the AI analyst, which can list them) sees the same story.
 *
 * Four decisions:
 *
 *   1. A NOTE BELONGS TO A MOMENT, NOT TO A PIXEL. It is stored as a time
 *      (`starts_at`, and `ends_at` for a range) and placed on whichever sample
 *      holds that time: the last bucket whose start is at or before it. The
 *      same note therefore lands on the right day at a 90-day range and on the
 *      right hour at 24 hours.
 *   2. IT LOOKS LIKE A NOTE, NOT LIKE DATA. The chart draws a person's note as
 *      a dashed rule with a flag (see Drum.tsx), and the automatic "steepest
 *      rise" notes as a dot with a dotted leader, so the two never read as the
 *      same kind of claim.
 *   3. THREE COLOURS, NONE OF THEM AN AUDIENCE. Neutral, green and red only.
 *      Blue means people and orange means AI everywhere in this product, and a
 *      note is neither.
 *   4. THE SERVER DECIDES WHO MAY WRITE. Everyone who can read the site sees
 *      the notes. The controls to add, change and delete them are always
 *      offered, and when the reader's role does not allow it the server's own
 *      sentence is shown beside the control that was refused.
 */
import { useEffect, useId, useMemo, useRef, useState } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";

import { api } from "../../lib/api";
import type { Annotation, AnnotationBody, AnnotationColor } from "../../lib/api";
import type { DrumAnnotation } from "../../components/react/Drum";
import { Button, Disclosure } from "../../components/react/ui";
import { IconCheck, IconClose, IconOut } from "../../components/react/icons";
import { errorDetail, rangeEnvelope } from "./TopSheets";
import "../../styles/app/annotations.css";

/* ── colours ─────────────────────────────────────────────────────────────── */

export const ANNOTATION_COLORS: { value: AnnotationColor; label: string; ink: string }[] = [
  { value: "neutral", label: "Neutral", ink: "var(--mf-ink-2)" },
  { value: "green", label: "Green", ink: "var(--mf-success)" },
  { value: "red", label: "Red", ink: "var(--mf-danger)" },
];

const inkOf = (c: AnnotationColor): string =>
  ANNOTATION_COLORS.find((x) => x.value === c)?.ink ?? "var(--mf-ink-2)";

/* ── reading them ────────────────────────────────────────────────────────── */

/** The notes overlapping the window being read, oldest first. */
export function useAnnotations(siteId: number | null | undefined, range: string, timezone?: string) {
  return useQuery({
    queryKey: ["annotations", siteId ?? null, range],
    queryFn: () =>
      api
        .get<Annotation[]>("/api/annotations", {
          site: siteId as number,
          ...rangeEnvelope(range),
          ...(timezone ? { tz: timezone } : {}),
        })
        .then((r) => r.data),
    enabled: siteId != null,
    staleTime: 30_000,
  });
}

/** The last bucket whose start is at or before `t`, or -1 when `t` is before them all. */
export function bucketIndex(times: number[], t: number): number {
  let lo = 0;
  let hi = times.length - 1;
  let found = -1;
  while (lo <= hi) {
    const mid = (lo + hi) >> 1;
    if (times[mid] <= t) {
      found = mid;
      lo = mid + 1;
    } else {
      hi = mid - 1;
    }
  }
  return found;
}

/**
 * Places each note on the chart's samples. A note that started before the
 * window but runs into it is drawn from the first sample; a note wholly
 * outside the window is left off, because the server's window and the
 * chart's buckets can differ by a bucket at the edges.
 */
export function annotationMarks(times: number[], notes: Annotation[]): DrumAnnotation[] {
  if (!times.length) return [];
  const step = times.length > 1 ? (times[times.length - 1] - times[0]) / (times.length - 1) : 0;
  const windowEnd = times[times.length - 1] + step;
  const out: DrumAnnotation[] = [];
  for (const a of notes) {
    const s = Date.parse(a.starts_at);
    const e = a.ends_at ? Date.parse(a.ends_at) : null;
    if (!Number.isFinite(s) || s >= windowEnd) continue;
    let at = bucketIndex(times, s);
    const to = e != null && Number.isFinite(e) ? bucketIndex(times, e) : null;
    if (at < 0) {
      if (to == null || to < 0) continue;
      at = 0;
    }
    out.push({
      at,
      to: to != null && to > at ? to : undefined,
      label: a.text,
      kind: "user",
      color: inkOf(a.color),
      id: a.id,
    });
  }
  return out;
}

/* ── times, for people and for the date field ────────────────────────────── */

const pad = (n: number) => String(n).padStart(2, "0");

/** A time as a `datetime-local` value, in the reader's own zone. */
export function toLocalInput(ms: number): string {
  const d = new Date(ms);
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}T${pad(d.getHours())}:${pad(d.getMinutes())}`;
}

/** A `datetime-local` value back to an ISO time, or null when it is not one. */
function fromLocalInput(v: string): string | null {
  if (!v) return null;
  const t = new Date(v).getTime();
  return Number.isFinite(t) ? new Date(t).toISOString() : null;
}

function whenWords(a: Annotation): string {
  const f = (iso: string) =>
    new Date(iso).toLocaleString(undefined, {
      day: "numeric",
      month: "short",
      hour: "2-digit",
      minute: "2-digit",
    });
  return a.ends_at ? `${f(a.starts_at)} to ${f(a.ends_at)}` : f(a.starts_at);
}

/** The flag a note is drawn with on the chart, in its own colour. */
function Flag({ color }: { color: AnnotationColor }) {
  return (
    <svg className="mf-ann__flag" width="12" height="14" viewBox="0 0 12 14" aria-hidden="true" focusable="false">
      <path
        d="M2 13V1.5h8l-2.4 3 2.4 3H2"
        fill={inkOf(color)}
        stroke={inkOf(color)}
        strokeWidth="1.2"
        strokeLinejoin="round"
        strokeLinecap="round"
      />
    </svg>
  );
}

/* ── the form ────────────────────────────────────────────────────────────── */

export interface AnnotationDraft {
  /** Set when an existing note is being changed. */
  id?: string;
  start: string;
  end: string;
  text: string;
  color: AnnotationColor;
  category: string;
}

export const draftFrom = (a: Annotation): AnnotationDraft => ({
  id: a.id,
  start: toLocalInput(Date.parse(a.starts_at)),
  end: a.ends_at ? toLocalInput(Date.parse(a.ends_at)) : "",
  text: a.text,
  color: a.color,
  category: a.category ?? "",
});

export const blankDraft = (startMs: number): AnnotationDraft => ({
  start: toLocalInput(startMs),
  end: "",
  text: "",
  color: "neutral",
  category: "",
});

const TEXT_MAX = 280;
const CATEGORY_MAX = 40;

export function AnnotationForm({
  siteId,
  draft,
  onDraft,
  onDone,
}: {
  siteId: number;
  draft: AnnotationDraft;
  onDraft: (d: AnnotationDraft) => void;
  onDone: () => void;
}) {
  const qc = useQueryClient();
  const ids = { start: useId(), end: useId(), text: useId(), category: useId() };
  const [tried, setTried] = useState(false);

  /* The form opens below the chart, and from the list's Edit button it opens
     above the row that was pressed, so the words field takes the focus: the
     reader lands where they are expected to type, by mouse or by keyboard. */
  const textRef = useRef<HTMLInputElement | null>(null);
  useEffect(() => {
    textRef.current?.focus();
  }, []);

  const startIso = fromLocalInput(draft.start);
  const endIso = fromLocalInput(draft.end);
  const problems = {
    start: startIso ? null : "Choose when this happened.",
    end: draft.end && !endIso ? "This end time is not a date." : endIso && startIso && endIso < startIso ? "The end is before the start." : null,
    text: !draft.text.trim() ? "Write what happened." : draft.text.length > TEXT_MAX ? `Keep it to ${TEXT_MAX} characters.` : null,
    category: draft.category.length > CATEGORY_MAX ? `Keep the category to ${CATEGORY_MAX} characters.` : null,
  };
  const valid = !problems.start && !problems.end && !problems.text && !problems.category;

  const save = useMutation({
    mutationFn: () => {
      const body: AnnotationBody = {
        starts_at: startIso as string,
        ends_at: endIso,
        text: draft.text.trim(),
        color: draft.color,
        category: draft.category.trim() || undefined,
      };
      return draft.id
        ? api.patch<Annotation>(`/api/annotations/${draft.id}`, body)
        : api.post<Annotation>(`/api/annotations?site=${encodeURIComponent(String(siteId))}`, body);
    },
    onSuccess: () => {
      void qc.invalidateQueries({ queryKey: ["annotations", siteId] });
      onDone();
    },
  });

  const set = (patch: Partial<AnnotationDraft>) => onDraft({ ...draft, ...patch });
  const show = (k: keyof typeof problems) => (tried ? problems[k] : null);

  return (
    <form
      className="mf-ann-form"
      aria-label={draft.id ? "Change this annotation" : "Add an annotation"}
      onSubmit={(e) => {
        e.preventDefault();
        setTried(true);
        if (valid && !save.isPending) save.mutate();
      }}
    >
      <p className="mf-ann-form__title">{draft.id ? "Change this annotation" : "Add an annotation"}</p>
      <p className="mf-ann-form__hint">
        Select a point on the chart to set the time, or type it. Everyone who can read this site sees
        the note.
      </p>

      <div className="mf-ann-form__grid">
        <div className="mf-ann-field">
          <label className="mf-ann-field__label" htmlFor={ids.start}>
            When
          </label>
          <input
            id={ids.start}
            type="datetime-local"
            className="mf-set-input mf-set-input--data"
            value={draft.start}
            aria-invalid={show("start") ? true : undefined}
            onChange={(e) => set({ start: e.target.value })}
          />
          {show("start") ? <p className="mf-ann-field__err">{show("start")}</p> : null}
        </div>
        <div className="mf-ann-field">
          <label className="mf-ann-field__label" htmlFor={ids.end}>
            Until <span className="mf-ann-field__opt">(optional, for a range)</span>
          </label>
          <input
            id={ids.end}
            type="datetime-local"
            className="mf-set-input mf-set-input--data"
            value={draft.end}
            min={draft.start || undefined}
            aria-invalid={show("end") ? true : undefined}
            onChange={(e) => set({ end: e.target.value })}
          />
          {show("end") ? <p className="mf-ann-field__err">{show("end")}</p> : null}
        </div>
      </div>

      <div className="mf-ann-field">
        <label className="mf-ann-field__label" htmlFor={ids.text}>
          What happened
        </label>
        <input
          id={ids.text}
          ref={textRef}
          className="mf-set-input"
          value={draft.text}
          maxLength={TEXT_MAX}
          placeholder="Launched the new pricing page"
          aria-invalid={show("text") ? true : undefined}
          onChange={(e) => set({ text: e.target.value })}
        />
        <p className="mf-ann-field__count mf-data" aria-live="polite">
          {draft.text.length} / {TEXT_MAX}
        </p>
        {show("text") ? <p className="mf-ann-field__err">{show("text")}</p> : null}
      </div>

      <div className="mf-ann-form__grid">
        <fieldset className="mf-ann-field mf-ann-colors">
          <legend className="mf-ann-field__label">Colour</legend>
          <div className="mf-ann-colors__row">
            {ANNOTATION_COLORS.map((c) => (
              <button
                key={c.value}
                type="button"
                className="mf-ann-colors__key"
                aria-pressed={draft.color === c.value}
                onClick={() => set({ color: c.value })}
              >
                <Flag color={c.value} />
                {c.label}
              </button>
            ))}
          </div>
        </fieldset>
        <div className="mf-ann-field">
          <label className="mf-ann-field__label" htmlFor={ids.category}>
            Category <span className="mf-ann-field__opt">(optional)</span>
          </label>
          <input
            id={ids.category}
            className="mf-set-input"
            value={draft.category}
            maxLength={CATEGORY_MAX}
            placeholder="Release"
            aria-invalid={show("category") ? true : undefined}
            onChange={(e) => set({ category: e.target.value })}
          />
          {show("category") ? <p className="mf-ann-field__err">{show("category")}</p> : null}
        </div>
      </div>

      <div className="mf-ann-form__foot">
        <Button type="submit" tone="primary" size="sm" disabled={save.isPending}>
          {save.isPending ? "Saving…" : draft.id ? "Save the change" : "Add the annotation"}
        </Button>
        <Button size="sm" tone="quiet" onClick={onDone}>
          Cancel
        </Button>
        {save.isError ? (
          <span className="mf-ann-say" data-tone="warn" role="alert">
            {errorDetail(save.error)}
          </span>
        ) : null}
      </div>
    </form>
  );
}

/* ── the list ────────────────────────────────────────────────────────────── */

function NoteRow({
  note,
  siteId,
  onEdit,
}: {
  note: Annotation;
  siteId: number;
  onEdit: (a: Annotation) => void;
}) {
  const qc = useQueryClient();
  const [arming, setArming] = useState(false);
  const remove = useMutation({
    mutationFn: () => api.del<{ deleted: string }>(`/api/annotations/${note.id}`),
    onSuccess: () => void qc.invalidateQueries({ queryKey: ["annotations", siteId] }),
  });

  // A confirmation left armed is a trap for the next click; it disarms itself.
  useEffect(() => {
    if (!arming) return;
    const t = window.setTimeout(() => setArming(false), 5000);
    return () => window.clearTimeout(t);
  }, [arming]);

  return (
    <li className="mf-ann-row">
      <Flag color={note.color} />
      <div className="mf-ann-row__text">
        <p className="mf-ann-row__what">{note.text}</p>
        <p className="mf-ann-row__meta">
          <span className="mf-data">{whenWords(note)}</span>
          {note.category ? <span>{note.category}</span> : null}
          <span>
            {note.author?.name
              ? `by ${note.author.name}`
              : note.source === "mcp"
                ? "by an MCP client"
                : note.source === "api"
                  ? "by the API"
                  : note.source === "github"
                    ? "from GitHub"
                    : null}
          </span>
          {/* A note from the GitHub App links to the release, the deployed
              commit or the push it records. Only an https address is a link. */}
          {note.url && /^https:\/\//i.test(note.url) ? (
            <a className="mf-ann-row__link" href={note.url} target="_blank" rel="noreferrer noopener">
              Open on GitHub
              <IconOut size={11} />
            </a>
          ) : null}
        </p>
        {remove.isError ? (
          <p className="mf-ann-say" data-tone="warn" role="alert">
            {errorDetail(remove.error)}
          </p>
        ) : null}
      </div>
      <div className="mf-ann-row__acts">
        <Button size="sm" tone="quiet" onClick={() => onEdit(note)}>
          Edit
        </Button>
        {arming ? (
          <>
            <Button
              size="sm"
              tone="danger"
              icon={<IconCheck size={12} />}
              disabled={remove.isPending}
              onClick={() => {
                setArming(false);
                remove.mutate();
              }}
            >
              Delete it
            </Button>
            <Button size="sm" tone="quiet" icon={<IconClose size={12} />} onClick={() => setArming(false)}>
              Keep
            </Button>
          </>
        ) : (
          <Button size="sm" tone="quiet" disabled={remove.isPending} onClick={() => setArming(true)}>
            {remove.isPending ? "Deleting…" : "Delete"}
          </Button>
        )}
      </div>
    </li>
  );
}

/**
 * Every note in the window, under the chart. The chart can only write five
 * notes in its margin and none at phone width, so this list is where each one
 * can always be read in full, changed or removed.
 */
export function AnnotationList({
  query,
  siteId,
  onEdit,
}: {
  query: ReturnType<typeof useAnnotations>;
  siteId: number;
  onEdit: (a: Annotation) => void;
}) {
  const notes = useMemo(() => query.data ?? [], [query.data]);

  if (query.isPending) {
    return (
      <div className="mf-ann-list mf-ann-list--waiting" aria-busy="true" aria-live="polite">
        <span className="mf-sr">Loading annotations…</span>
        <span className="mf-wait" style={{ width: "38%" }} />
      </div>
    );
  }
  if (query.isError) {
    return (
      <p className="mf-ann-list mf-ann-say" data-tone="warn" role="alert">
        Could not load the annotations. {errorDetail(query.error)}{" "}
        <button type="button" className="mf-ann-retry" onClick={() => void query.refetch()}>
          Try again
        </button>
      </p>
    );
  }
  if (!notes.length) {
    return (
      <p className="mf-ann-list mf-ann-say">
        No annotations in this period. Add one to mark a release, a campaign or an outage on the
        chart.
      </p>
    );
  }
  return (
    <div className="mf-ann-list">
      <Disclosure
        summary={
          <span>
            Annotations in this period <span className="mf-data">({notes.length})</span>
          </span>
        }
      >
        <ul className="mf-ann-rows">
          {notes.map((a) => (
            <NoteRow key={a.id} note={a} siteId={siteId} onEdit={onEdit} />
          ))}
        </ul>
      </Disclosure>
    </div>
  );
}
