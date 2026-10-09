/**
 * A small tokenizer for HTML-shaped text: HTML pages, Vue templates, Svelte components,
 * Astro pages, and HTML written inside a JavaScript string.
 *
 * It finds start tags with their attributes and the contents of `<script>` blocks, and
 * nothing else. It is not an HTML parser and does not need to be: the scanner asks
 * "which elements carry `data-track`, and what code sits in attributes and scripts", and
 * every one of those questions is answered by the start tags. A real parser for each of
 * the four template languages would be four dependencies to answer it.
 *
 * Offsets are into the text as given, so the caller can turn them into lines.
 */

/** One attribute of a start tag. */
export interface TagAttr {
  /** As written: `data-track`, `:data-track`, `@click`, `on:click`, `use:trackClick`. */
  name: string;
  /** The value without its quotes or braces; `null` for a bare attribute. */
  value: string | null;
  /** How the value was delimited: a quote, `{` for a Svelte or Astro expression, or none. */
  quote: '"' | "'" | "{" | null;
  /** Offset of the attribute's first character. */
  start: number;
  /** Offset of the value's first character, inside the quotes or braces. */
  valueStart: number;
}

/** One start tag. */
export interface Tag {
  /** The element name, as written. */
  name: string;
  attrs: TagAttr[];
  /** Offset of the `<`. */
  start: number;
}

/** The contents of one `<script>` block. */
export interface ScriptBlock {
  tag: Tag;
  content: string;
  /** Offset of the content's first character. */
  contentStart: number;
}

/** What one pass over the text found. */
export interface Markup {
  tags: Tag[];
  scripts: ScriptBlock[];
}

/** Index after the quote that closes the string opening at `i`. */
function skipString(text: string, i: number, quote: string): number {
  for (let j = i + 1; j < text.length; j++) {
    if (text[j] === "\\") j++;
    else if (text[j] === quote) return j + 1;
  }
  return text.length;
}

/** Index after the backtick that closes the template literal opening at `i`. */
function skipTemplate(text: string, i: number): number {
  for (let j = i + 1; j < text.length; j++) {
    const c = text[j];
    if (c === "\\") j++;
    else if (c === "`") return j + 1;
    else if (c === "$" && text[j + 1] === "{") j = skipBraces(text, j + 1) - 1;
  }
  return text.length;
}

/** Index after the `}` that balances the `{` at `i`, stepping over strings in between. */
export function skipBraces(text: string, i: number): number {
  let depth = 0;
  for (let j = i; j < text.length; j++) {
    const c = text[j];
    if (c === '"' || c === "'") j = skipString(text, j, c) - 1;
    else if (c === "`") j = skipTemplate(text, j) - 1;
    else if (c === "{") depth++;
    else if (c === "}" && --depth === 0) return j + 1;
  }
  return text.length;
}

const isSpace = (c: string | undefined) => c === " " || c === "\n" || c === "\t" || c === "\r" || c === "\f";

/**
 * Scan `text` for start tags and script blocks.
 *
 * `braces` turns on Svelte and Astro attribute values written as `{expression}`, and
 * their `{...spread}` attributes, which are skipped.
 */
export function scanMarkup(text: string, braces: boolean): Markup {
  const tags: Tag[] = [];
  const scripts: ScriptBlock[] = [];
  const lower = text.toLowerCase();
  let i = 0;
  while (i < text.length) {
    const lt = text.indexOf("<", i);
    if (lt === -1) break;
    if (text.startsWith("<!--", lt)) {
      const close = text.indexOf("-->", lt + 4);
      i = close === -1 ? text.length : close + 3;
      continue;
    }
    if (!/[A-Za-z]/.test(text[lt + 1] ?? "")) {
      i = lt + 1;
      continue;
    }

    let j = lt + 1;
    while (j < text.length && /[\w:.-]/.test(text[j] as string)) j++;
    const tag: Tag = { name: text.slice(lt + 1, j), attrs: [], start: lt };

    while (j < text.length) {
      while (isSpace(text[j])) j++;
      const c = text[j];
      if (c === undefined || c === ">") {
        j++;
        break;
      }
      if (c === "/" && text[j + 1] === ">") {
        j += 2;
        break;
      }
      if (braces && c === "{") {
        j = skipBraces(text, j);
        continue;
      }
      const nameStart = j;
      while (j < text.length && !isSpace(text[j]) && text[j] !== "=" && text[j] !== ">") {
        if (text[j] === "/" && text[j + 1] === ">") break;
        j++;
      }
      const name = text.slice(nameStart, j);
      if (!name) {
        j++;
        continue;
      }
      let k = j;
      while (isSpace(text[k])) k++;
      if (text[k] !== "=") {
        tag.attrs.push({ name, value: null, quote: null, start: nameStart, valueStart: j });
        continue;
      }
      k++;
      while (isSpace(text[k])) k++;
      const q = text[k];
      if (q === '"' || q === "'") {
        const close = text.indexOf(q, k + 1);
        const end = close === -1 ? text.length : close;
        tag.attrs.push({ name, value: text.slice(k + 1, end), quote: q, start: nameStart, valueStart: k + 1 });
        j = end + 1;
      } else if (braces && q === "{") {
        const end = skipBraces(text, k);
        tag.attrs.push({ name, value: text.slice(k + 1, end - 1), quote: "{", start: nameStart, valueStart: k + 1 });
        j = end;
      } else {
        let end = k;
        while (end < text.length && !isSpace(text[end]) && text[end] !== ">") end++;
        tag.attrs.push({ name, value: text.slice(k, end), quote: null, start: nameStart, valueStart: k });
        j = end;
      }
    }
    tags.push(tag);

    const element = tag.name.toLowerCase();
    if (element === "script" || element === "style") {
      const close = lower.indexOf(`</${element}`, j);
      const end = close === -1 ? text.length : close;
      if (element === "script") scripts.push({ tag, content: text.slice(j, end), contentStart: j });
      i = end;
    } else i = j;
  }
  return { tags, scripts };
}

/** Turns offsets into 1-based lines and columns. */
export class LineIndex {
  private readonly starts: number[] = [0];

  constructor(text: string) {
    for (let i = 0; i < text.length; i++) if (text.charCodeAt(i) === 10) this.starts.push(i + 1);
  }

  /** The 1-based line and column of `offset`. */
  at(offset: number): { line: number; column: number } {
    let lo = 0;
    let hi = this.starts.length - 1;
    while (lo < hi) {
      const mid = (lo + hi + 1) >> 1;
      if ((this.starts[mid] as number) <= offset) lo = mid;
      else hi = mid - 1;
    }
    return { line: lo + 1, column: offset - (this.starts[lo] as number) + 1 };
  }
}
