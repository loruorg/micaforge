/**
 * Text a model wrote, drawn as plain paragraphs and simple lists.
 *
 * Models answer in loose Markdown. The dashboard does not hand that to a
 * Markdown library, and it never sets it as HTML: an answer is built from the
 * traffic of a public website, so a page title or a referrer a stranger chose
 * can end up inside it, and anything treated as markup would be an injection
 * door. Instead the few shapes an answer actually uses are read here and drawn
 * as React elements, so every character reaches the page as text:
 *
 *   - blank lines split paragraphs;
 *   - lines starting "- ", "* " or "• " make a list, "1. " or "1) " a numbered one;
 *   - "# Heading" lines become a short bold line, not a page heading, because
 *     the screen already owns the heading levels;
 *   - fenced ``` blocks keep their spacing in the data face;
 *   - **bold** and `code` are kept inside a line; a [label](address) link is
 *     written out as "label (address)" and is never made clickable.
 *
 * Everything else is shown exactly as written.
 */
import type { ReactNode } from "react";

type Block =
  | { kind: "p"; text: string }
  | { kind: "head"; text: string }
  | { kind: "ul"; items: string[] }
  | { kind: "ol"; items: string[] }
  | { kind: "code"; text: string };

const BULLET = /^\s*[-*•]\s+(.*)$/;
const NUMBERED = /^\s*\d{1,3}[.)]\s+(.*)$/;
const HEADING = /^\s*#{1,6}\s+(.*)$/;
const FENCE = /^\s*```/;

/** Reads the text into blocks. Pure, so the same answer always draws the same way. */
export function readBlocks(text: string): Block[] {
  const lines = text.replace(/\r\n?/g, "\n").split("\n");
  const out: Block[] = [];
  let para: string[] = [];
  // Asserted rather than annotated: the helpers below reassign it, and an
  // annotated `= null` would let the compiler narrow it to `null` for good.
  let list = null as { kind: "ul" | "ol"; items: string[] } | null;

  const flushPara = () => {
    if (para.length) out.push({ kind: "p", text: para.join(" ") });
    para = [];
  };
  const flushList = () => {
    if (list) out.push(list);
    list = null;
  };

  for (let i = 0; i < lines.length; i++) {
    const line = lines[i];

    if (FENCE.test(line)) {
      flushPara();
      flushList();
      const code: string[] = [];
      i++;
      while (i < lines.length && !FENCE.test(lines[i])) {
        code.push(lines[i]);
        i++;
      }
      out.push({ kind: "code", text: code.join("\n") });
      continue;
    }

    if (!line.trim()) {
      flushPara();
      flushList();
      continue;
    }

    const head = HEADING.exec(line);
    if (head) {
      flushPara();
      flushList();
      out.push({ kind: "head", text: head[1] });
      continue;
    }

    const bullet = BULLET.exec(line);
    const numbered = bullet ? null : NUMBERED.exec(line);
    if (bullet || numbered) {
      flushPara();
      const kind = bullet ? "ul" : "ol";
      if (!list || list.kind !== kind) {
        flushList();
        list = { kind, items: [] };
      }
      list.items.push((bullet ?? numbered)![1]);
      continue;
    }

    // A wrapped line under a list item continues that item.
    if (list && /^\s{2,}\S/.test(line)) {
      const items = list.items;
      items[items.length - 1] = `${items[items.length - 1]} ${line.trim()}`;
      continue;
    }

    flushList();
    para.push(line.trim());
  }
  flushPara();
  flushList();
  return out;
}

/** **bold**, `code` and [label](address) inside one line, as elements. */
function inline(text: string): ReactNode[] {
  const out: ReactNode[] = [];
  const pattern = /(\*\*[^*]+\*\*|`[^`]+`|\[[^\]]+\]\([^)\s]+\))/g;
  let last = 0;
  let k = 0;
  for (let m = pattern.exec(text); m; m = pattern.exec(text)) {
    if (m.index > last) out.push(text.slice(last, m.index));
    const token = m[0];
    if (token.startsWith("**")) {
      out.push(<strong key={k++}>{token.slice(2, -2)}</strong>);
    } else if (token.startsWith("`")) {
      out.push(
        <code key={k++} className="mf-plain__code mf-data">
          {token.slice(1, -1)}
        </code>,
      );
    } else {
      const cut = token.indexOf("](");
      out.push(`${token.slice(1, cut)} (${token.slice(cut + 2, -1)})`);
    }
    last = m.index + token.length;
  }
  if (last < text.length) out.push(text.slice(last));
  return out;
}

export function PlainText({ text, className }: { text: string; className?: string }) {
  const blocks = readBlocks(text);
  return (
    <div className={className ? `mf-plain ${className}` : "mf-plain"}>
      {blocks.map((b, i) => {
        switch (b.kind) {
          case "p":
            return <p key={i}>{inline(b.text)}</p>;
          case "head":
            return (
              <p key={i} className="mf-plain__head">
                {inline(b.text)}
              </p>
            );
          case "ul":
            return (
              <ul key={i}>
                {b.items.map((item, j) => (
                  <li key={j}>{inline(item)}</li>
                ))}
              </ul>
            );
          case "ol":
            return (
              <ol key={i}>
                {b.items.map((item, j) => (
                  <li key={j}>{inline(item)}</li>
                ))}
              </ol>
            );
          case "code":
            return (
              <pre key={i} className="mf-plain__pre" tabIndex={0}>
                {b.text}
              </pre>
            );
        }
      })}
    </div>
  );
}

export default PlainText;
