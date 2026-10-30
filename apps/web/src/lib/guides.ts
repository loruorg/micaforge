/**
 * The task guides' shape.
 *
 * One ordering, one URL rule and one set of structured data, shared by the guide list,
 * each guide, the left index and previous and next, so none of them can disagree about
 * what the guides are or where they live.
 */
import type { CollectionEntry } from "astro:content";
import { TITLE_SUFFIX } from "../content.config";
import { isoDay } from "./docs";

export type GuideEntry = CollectionEntry<"guides">;

/** The list's own address. Trailing slash, which is the site's canonical form. */
export const GUIDES_HREF = "/docs/guides/";

/** One guide's address. */
export const guideHref = (id: string): string => `${GUIDES_HREF}${id}/`;

/** The `<title>` of a guide. The schema holds it under 60 characters. */
export const guideTitle = (g: GuideEntry): string => `${g.data.seoTitle ?? g.data.title}${TITLE_SUFFIX}`;

/** Published guides, in reading order. Drafts never ship. */
export function ordered(entries: GuideEntry[]): GuideEntry[] {
  return entries
    .filter((g) => !g.data.draft)
    .sort((a, b) => a.data.order - b.data.order || a.data.title.localeCompare(b.data.title));
}

/** A link the docs layout can draw as previous or next. */
export interface Turn {
  href: string;
  title: string;
}

/** The guides either side of `id`. Previous and next stay inside the guides. */
export function guideNeighbours(entries: GuideEntry[], id: string): { prev?: Turn; next?: Turn } {
  const list = ordered(entries);
  const i = list.findIndex((g) => g.id === id);
  if (i < 0) return {};
  const turn = (g: GuideEntry | undefined): Turn | undefined =>
    g ? { href: guideHref(g.id), title: g.data.title } : undefined;
  return { prev: turn(list[i - 1]), next: turn(list[i + 1]) };
}

/** The related guides a page names. An id that names no guide fails the build. */
export function relatedOf(entries: GuideEntry[], guide: GuideEntry): GuideEntry[] {
  const live = ordered(entries);
  return guide.data.related.map((id) => {
    const found = live.find((g) => g.id === id);
    if (!found) throw new Error(`guides/${guide.id}: related guide "${id}" does not exist`);
    if (found.id === guide.id) throw new Error(`guides/${guide.id}: a guide cannot be related to itself`);
    return found;
  });
}

const PUBLISHER = {
  "@type": "Organization",
  name: "Micaforge",
  url: "https://micaforge.com/",
  logo: "https://micaforge.com/mark.svg",
};

/** Home, Docs, Guides, then whatever `tail` adds. */
function breadcrumbs(site: URL, tail: { name: string; href: string }[]) {
  const trail = [
    { name: "Home", href: "/" },
    { name: "Docs", href: "/docs/" },
    { name: "Guides", href: GUIDES_HREF },
    ...tail,
  ];
  return {
    "@type": "BreadcrumbList",
    itemListElement: trail.map((step, i) => ({
      "@type": "ListItem",
      position: i + 1,
      name: step.name,
      item: new URL(step.href, site).href,
    })),
  };
}

/** TechArticle and BreadcrumbList for one guide. */
export function guideJsonLd(site: URL, guide: GuideEntry) {
  const url = new URL(guideHref(guide.id), site).href;
  const day = isoDay(guide.data.updated);
  return {
    "@context": "https://schema.org",
    "@graph": [
      {
        "@type": "TechArticle",
        "@id": `${url}#article`,
        headline: guide.data.title,
        description: guide.data.description,
        url,
        mainEntityOfPage: url,
        inLanguage: "en",
        datePublished: day,
        dateModified: day,
        image: new URL("/og/default.png", site).href,
        author: PUBLISHER,
        publisher: PUBLISHER,
        isPartOf: { "@type": "CollectionPage", "@id": `${new URL(GUIDES_HREF, site).href}#list` },
      },
      breadcrumbs(site, [{ name: guide.data.title, href: guideHref(guide.id) }]),
    ],
  };
}

/** CollectionPage with its ItemList, and the BreadcrumbList, for the guide list. */
export function guidesIndexJsonLd(site: URL, guides: GuideEntry[], name: string, description: string) {
  const url = new URL(GUIDES_HREF, site).href;
  const latest = guides.reduce((d, g) => (g.data.updated > d ? g.data.updated : d), new Date(0));
  return {
    "@context": "https://schema.org",
    "@graph": [
      {
        "@type": "CollectionPage",
        "@id": `${url}#list`,
        name,
        description,
        url,
        inLanguage: "en",
        dateModified: isoDay(latest),
        publisher: PUBLISHER,
        mainEntity: {
          "@type": "ItemList",
          numberOfItems: guides.length,
          itemListElement: guides.map((g, i) => ({
            "@type": "ListItem",
            position: i + 1,
            name: g.data.title,
            url: new URL(guideHref(g.id), site).href,
          })),
        },
      },
      breadcrumbs(site, []),
    ],
  };
}
