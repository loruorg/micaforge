/**
 * The manual's shape.
 *
 * The section list, its labels, and the ordering rules live here so that the index page,
 * the left nav and the previous/next links can never disagree about what the manual
 * contains or what order it is read in.
 */
import type { CollectionEntry } from "astro:content";

export type DocEntry = CollectionEntry<"docs">;

export interface DocGroup {
  /** Directory name, and the `group` value in every page's frontmatter. */
  key: DocEntry["data"]["group"];
  /** What the section is called in the index and the nav. */
  label: string;
  /** One line, shown on the manual's contents sheet. Never a marketing sentence. */
  gloss: string;
}

/** Reading order, top to bottom. */
export const DOC_GROUPS: DocGroup[] = [
  {
    key: "getting-started",
    label: "Getting started",
    gloss: "From an empty site to a first pageview.",
  },
  {
    key: "tracking",
    label: "Tracking",
    gloss: "What the browser reports, and what it does not.",
  },
  {
    key: "agents",
    label: "AI traffic",
    gloss: "AI crawlers and AI referrals: the traffic a script tag cannot see.",
  },
  {
    key: "dashboard",
    label: "The dashboard",
    gloss: "Each screen, and the question it answers.",
  },
  {
    key: "api",
    label: "The API",
    gloss: "Read your own data, or let an agent read it.",
  },
  {
    key: "self-hosting",
    label: "Self-hosting",
    gloss: "One machine, five containers, no outbound call.",
  },
  {
    key: "migrating",
    label: "Migrating",
    gloss: "Bringing history over from another tool.",
  },
  {
    key: "privacy",
    label: "Privacy",
    gloss: "What is stored, and what is never stored.",
  },
  {
    key: "integrations",
    label: "Integrations",
    gloss: "Chat apps that answer questions and post what changed.",
  },
];

export interface DocSection {
  group: DocGroup;
  docs: DocEntry[];
}

/** Drafts never ship, in any environment. A half-written page is not documentation. */
export const published = (entries: DocEntry[]): DocEntry[] =>
  entries.filter((e) => !e.data.draft);

const byOrder = (a: DocEntry, b: DocEntry): number =>
  a.data.order - b.data.order || a.data.title.localeCompare(b.data.title);

/** The manual, grouped and ordered. Empty sections are dropped rather than shown bare. */
export function sections(entries: DocEntry[]): DocSection[] {
  const live = published(entries);
  return DOC_GROUPS.map((group) => ({
    group,
    docs: live.filter((e) => e.data.group === group.key).sort(byOrder),
  })).filter((s) => s.docs.length > 0);
}

/** One flat reading order, which is what previous and next actually mean. */
export const reading = (entries: DocEntry[]): DocEntry[] =>
  sections(entries).flatMap((s) => s.docs);

export interface Neighbours {
  prev?: DocEntry;
  next?: DocEntry;
}

/** The pages either side of `id` in reading order. */
export function neighbours(entries: DocEntry[], id: string): Neighbours {
  const flat = reading(entries);
  const i = flat.findIndex((e) => e.id === id);
  if (i < 0) return {};
  return { prev: flat[i - 1], next: flat[i + 1] };
}

/** A docs URL. One definition, so a rename cannot leave half the links behind. */
export const docHref = (id: string): string => `/docs/${id}`;

/** `2026-08-22`. The data face renders these, so they are held to one width. */
export const isoDay = (d: Date): string => d.toISOString().slice(0, 10);

/** `22 August 2026`, for prose. */
export const longDay = (d: Date): string =>
  d.toLocaleDateString("en-GB", { day: "numeric", month: "long", year: "numeric", timeZone: "UTC" });
