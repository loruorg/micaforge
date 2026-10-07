/**
 * Content collections.
 *
 * Three collections, three jobs: the manual (`docs`), the argument (`blog`) and the
 * record of what changed (`changelog`). All three are file-backed and static. There is
 * no CMS, no API call at build time, and nothing here reaches the network.
 *
 * The schemas are deliberately strict. A page that forgets its `group` would silently
 * fall out of the index rather than fail the build, and a docs site that quietly loses a
 * page is worse than one that will not compile.
 */
import { defineCollection } from "astro:content";
import { glob } from "astro/loaders";
// zod directly rather than the re-export from `astro:content`, which is deprecated.
import { z } from "zod";

/** The eight sections of the manual, in reading order. Also the directory names. */
export const DOC_GROUP_KEYS = [
  "getting-started",
  "tracking",
  "agents",
  "dashboard",
  "api",
  "self-hosting",
  "migrating",
  "privacy",
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

export const collections = { docs, blog, changelog };
