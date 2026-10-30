/**
 * Content collections.
 *
 * Four collections, four jobs: the manual (`docs`), the task guides (`guides`), the
 * argument (`blog`) and the record of what changed (`changelog`). All four are
 * file-backed and static. There is no CMS, no API call at build time, and nothing here
 * reaches the network.
 *
 * The schemas are deliberately strict. A page that forgets its `group` would silently
 * fall out of the index rather than fail the build, and a docs site that quietly loses a
 * page is worse than one that will not compile.
 */
import { defineCollection } from "astro:content";
import { glob } from "astro/loaders";
// zod directly rather than the re-export from `astro:content`, which is deprecated.
import { z } from "zod";

/** The sections of the manual, in reading order. Also the directory names. */
export const DOC_GROUP_KEYS = [
  "getting-started",
  "tracking",
  "agents",
  "dashboard",
  "api",
  "self-hosting",
  "migrating",
  "privacy",
  "integrations",
] as const;

const docs = defineCollection({
  loader: glob({ base: "./src/content/docs", pattern: "**/*.mdx" }),
  schema: z.object({
    /** Sentence case, and short enough to sit in the left index without wrapping twice. */
    title: z.string().max(60),
    /** One line. Rendered under the title, and used as the page description. */
    description: z.string().max(180),
    /** Position inside the group. Ties fall back to the title. */
    order: z.number().int().default(100),
    /** Which section of the manual this belongs to. Must match the directory. */
    group: z.enum(DOC_GROUP_KEYS),
    /** When the page was last checked against the code it documents. */
    updated: z.coerce.date().optional(),
    /** Written, not published. Excluded from every build. */
    draft: z.boolean().default(false),
  }),
});

const blog = defineCollection({
  loader: glob({ base: "./src/content/blog", pattern: "**/*.mdx" }),
  schema: z.object({
    title: z.string().max(90),
    description: z.string().max(220),
    /** Publication date. Drives the order of the index. */
    updated: z.coerce.date(),
    /** Ordering hint when two posts share a date. Lower is earlier. */
    order: z.number().int().default(100),
    group: z.string().default("post"),
    draft: z.boolean().default(false),
  }),
});

const changelog = defineCollection({
  loader: glob({ base: "./src/content/changelog", pattern: "**/*.mdx" }),
  schema: z.object({
    /** The version, exactly as it is tagged. */
    title: z.string().max(60),
    description: z.string().max(220),
    /** The date of the entry. */
    updated: z.coerce.date(),
    /** Newest first is `order` ascending. */
    order: z.number().int().default(100),
    /** `release` for a tagged version, `unreleased` for what is on main. */
    group: z.enum(["release", "unreleased"]).default("release"),
    draft: z.boolean().default(false),
  }),
});

/** What follows the page title in every `<title>`. */
export const TITLE_SUFFIX = " · Micaforge";

/**
 * Task guides: one job per page, start to finish, at `/docs/guides/<id>/`.
 *
 * A guide links to the manual for the reference and never repeats it. The limits below are
 * search limits rather than style: a `<title>` of 60 characters or more and a description
 * of 155 or more are cut short in a results page.
 */
const guides = defineCollection({
  loader: glob({ base: "./src/content/guides", pattern: "*.mdx" }),
  schema: z
    .object({
      /** The H1: the task, as the reader would type it. */
      title: z.string().max(80),
      /** The `<title>` before the suffix, when the H1 is too long for one. */
      seoTitle: z.string().optional(),
      /** The left index's label. Short enough for one line. */
      nav: z.string().max(34),
      /** The meta description, and the line under the title in the guide list. */
      description: z.string().max(154),
      /** Position in the list, and in previous and next. */
      order: z.number().int(),
      /** When the guide was last checked against the code and the live product. */
      updated: z.coerce.date(),
      /** Ids of two to four other guides. A wrong id fails the build. */
      related: z.array(z.string()).min(2).max(4),
      /** The closing line: a sentence, then one link. */
      cta: z.object({ text: z.string(), label: z.string(), href: z.string() }),
      draft: z.boolean().default(false),
    })
    .refine((g) => (g.seoTitle ?? g.title).length + TITLE_SUFFIX.length < 60, {
      message: "the <title> must be under 60 characters: set or shorten seoTitle",
      path: ["seoTitle"],
    }),
});

export const collections = { docs, blog, changelog, guides };
