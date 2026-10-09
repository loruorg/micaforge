/**
 * Finding analytics calls in Vue, Svelte, Astro and HTML files.
 *
 * Each of these is code inside markup. The code parts (every `<script>` block, an Astro
 * page's frontmatter, and the attributes that hold expressions) go to the JavaScript
 * extractor with their position in the file, so a line number in the report is a line
 * in the file. The scripts are read first, because they say what `track` means in the
 * template: `const { track } = useMicaforge()` in a `<script setup>` is what makes
 * `@click="track('signup')"` a call worth reporting.
 *
 * The markup itself contributes three things:
 *
 * - `data-track="name"` and its `data-track-*` properties, on any element, static or
 *   bound (`:data-track="name"` in Vue, `data-track={name}` in Svelte and Astro);
 * - Vue's `v-mf` directive and Svelte's `use:trackClick` action, whose value is a name
 *   or `{ event, props }`;
 * - attributes that hold code (`@click`, `v-on:click`, `:prop`, `on:click={…}`,
 *   `onclick="…"`), searched like any other code when they mention `track` or
 *   `micaforge`. The check keeps the scan from parsing every `class` binding in a large
 *   template.
 */
import {
  collectBindings,
  emit,
  findCalls,
  parseFragment,
  parseScript,
  propsOf,
  resolveNames,
  unwrap,
  type FileContext,
  type Node,
  type Resolved,
} from "./js.js";
import type { PropInfo } from "./model.js";
import { LineIndex, scanMarkup, type Tag, type TagAttr } from "./tags.js";

/** The template languages this module reads. */
export type MarkupKind = "vue" | "svelte" | "astro" | "html";

/** Script types that hold data or templates, not code. */
const NOT_CODE = /json|template|importmap|x-tmpl|text\/html|text\/plain|speculationrules/i;

/** An attribute worth parsing must at least mention one of these. */
const MENTIONS = /track|micaforge/i;

/** A parsed piece of code and the text it was parsed from. */
interface Parsed {
  ast: Node;
  code: string;
}

/** Where a tag is, for the report. */
type Where = { line: number; column: number };

/** Read one markup file into `ctx`. */
export function extractMarkup(ctx: FileContext, text: string, kind: MarkupKind): void {
  const lines = new LineIndex(text);
  const origin = (offset: number) => {
    const at = lines.at(offset);
    return { line: at.line, column: at.column - 1 };
  };
  const scriptsParsed: Parsed[] = [];
  let markup = text;

  // Astro frontmatter: TypeScript between two `---` fences at the top.
  if (kind === "astro") {
    const fence = /^\s*---\r?\n/.exec(text);
    const start = fence ? fence[0].length : -1;
    const close = fence ? text.indexOf("\n---", start) : -1;
    if (fence && close !== -1) {
      const code = text.slice(start, close + 1);
      const ast = parseScript(ctx, code, true, false, origin(start));
      if (ast) scriptsParsed.push({ ast, code });
      // Blanked rather than cut, so every offset after it still points into the file.
      markup = text.slice(0, close + 4).replace(/[^\n]/g, " ") + text.slice(close + 4);
    }
  }

  const { tags, scripts } = scanMarkup(markup, kind === "svelte" || kind === "astro");
  for (const block of scripts) {
    const attr = (name: string) => block.tag.attrs.find((a) => a.name.toLowerCase() === name)?.value ?? "";
    if (NOT_CODE.test(attr("type")) || !block.content.trim()) continue;
    const lang = attr("lang").toLowerCase();
    const typescript = kind === "astro" || lang === "ts" || lang === "tsx" || /typescript/i.test(attr("type"));
    const jsx = lang === "tsx" || lang === "jsx";
    const ast = parseScript(ctx, block.content, typescript, jsx, origin(block.contentStart));
    if (ast) scriptsParsed.push({ ast, code: block.content });
  }
  for (const { ast } of scriptsParsed) collectBindings(ctx, ast);
  for (const { ast, code } of scriptsParsed) findCalls(ctx, ast, code);

  const parseAt = (value: string, offset: number): Parsed | null => {
    const ast = parseFragment(value, origin(offset), kind !== "html");
    return ast ? { ast, code: value } : null;
  };
  for (const tag of tags) {
    const element = tag.name.toLowerCase();
    if (element !== "script" && element !== "style") readTag(ctx, tag, kind, lines.at(tag.start), parseAt);
  }
}

/** The plain name of an attribute: Vue's `:data-track` and `v-bind:data-track` are `data-track`. */
function plainName(attr: TagAttr, kind: MarkupKind): string {
  const name = attr.name.toLowerCase();
  return kind === "vue" ? name.replace(/^(?::|v-bind:)/, "") : name;
}

/** True when the value is an expression rather than text. */
function isBound(attr: TagAttr, kind: MarkupKind): boolean {
  return attr.quote === "{" || (kind === "vue" && plainName(attr, kind) !== attr.name.toLowerCase());
}

/** True when the attribute holds code that could call `track`. */
function holdsCode(attr: TagAttr, kind: MarkupKind): boolean {
  if (attr.value === null || !MENTIONS.test(attr.value)) return false;
  if (attr.quote === "{") return true;
  if (/^on[a-z]+$/i.test(attr.name)) return true;
  return kind === "vue" && /^(?:@|:|v-)/.test(attr.name);
}

/** The single expression of a value, whether it parsed as an expression or as statements. */
function expressionOf(piece: Parsed): Node | null {
  if (piece.ast.type !== "File") return unwrap(piece.ast);
  const body = piece.ast.program?.body ?? [];
  return body.length === 1 && body[0].type === "ExpressionStatement" ? unwrap(body[0].expression) : null;
}

/** One start tag. */
function readTag(
  ctx: FileContext,
  tag: Tag,
  kind: MarkupKind,
  where: Where,
  parseAt: (value: string, offset: number) => Parsed | null,
): void {
  const tracked = tag.attrs.find((a) => plainName(a, kind) === "data-track");
  if (tracked) dataTrackSite(ctx, tag, tracked, kind, where, parseAt);

  for (const attr of tag.attrs) {
    if (attr === tracked || plainName(attr, kind).startsWith("data-track-") || attr.value === null) continue;
    const name = attr.name;
    const directive =
      (kind === "vue" && /^v-mf(?:$|[:.])/.test(name)) ||
      (kind === "svelte" && name.startsWith("use:") && ctx.actions.has(name.slice(4)));
    if (directive) {
      const piece = parseAt(attr.value, attr.valueStart);
      if (piece) directiveSite(ctx, where, piece);
    } else if (holdsCode(attr, kind)) {
      const piece = parseAt(attr.value, attr.valueStart);
      if (piece) findCalls(ctx, piece.ast, piece.code);
    }
  }
}

/** `data-track` on an element, with its `data-track-*` properties. */
function dataTrackSite(
  ctx: FileContext,
  tag: Tag,
  tracked: TagAttr,
  kind: MarkupKind,
  where: Where,
  parseAt: (value: string, offset: number) => Parsed | null,
): void {
  let resolved: Resolved = { names: [tracked.value ?? ""] };
  if (isBound(tracked, kind) && tracked.value !== null) {
    const piece = parseAt(tracked.value, tracked.valueStart);
    resolved = piece
      ? resolveNames(ctx, expressionOf(piece), piece.code)
      : { expression: tracked.value.trim().slice(0, 80) };
  }
  const props: PropInfo[] = [];
  for (const attr of tag.attrs) {
    const name = plainName(attr, kind);
    if (!name.startsWith("data-track-")) continue;
    const key = name.slice(11);
    if (!isBound(attr, kind) && attr.value !== null) props.push({ key, type: "string", literal: attr.value });
    else props.push({ key, type: "string", ...(attr.value ? { expression: attr.value.trim().slice(0, 60) } : {}) });
  }
  emit(ctx, where, "attribute", resolved, { props, dynamic: false });
}

/** `v-mf="'name'"`, `v-mf="{ event: 'name', props: { … } }"`, or Svelte's `use:trackClick`. */
function directiveSite(ctx: FileContext, where: Where, piece: Parsed): void {
  const value = expressionOf(piece);
  if (!value) return;
  if (value.type === "ObjectExpression") {
    const field = (key: string) =>
      value.properties.find(
        (p: Node) => p.type === "ObjectProperty" && !p.computed && (p.key?.name === key || p.key?.value === key),
      )?.value as Node | undefined;
    const event = field("event");
    if (event) emit(ctx, where, "directive", resolveNames(ctx, event, piece.code), propsOf(ctx, field("props"), piece.code));
    return;
  }
  emit(ctx, where, "directive", resolveNames(ctx, value, piece.code), { props: [], dynamic: false });
}
