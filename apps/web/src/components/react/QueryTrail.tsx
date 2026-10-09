/**
 * The queries behind an AI answer: every tool the model ran, with the exact
 * reads it made.
 *
 * An answer from a model is a claim, and a claim about traffic is only worth
 * acting on when the figures in it can be traced to the read that produced
 * them. So each tool call is listed in the words the rest of the product uses:
 * the stats API request a person could send themselves, and the ClickHouse SQL
 * the server ran, with its parameters, exactly as executed. Nothing here is
 * paraphrased or summarised by the model; it is the server's own record.
 *
 * Three decisions:
 *
 *   1. CLOSED BY DEFAULT. The answer is what most readers came for. The trail
 *      is one click away and says how much is inside it, so it never pushes the
 *      answer off the screen and never hides that it exists.
 *   2. PARAMETERS STAY SEPARATE. The SQL is shown with its `?` placeholders and
 *      the parameters listed beside it, in order, because that is how it ran.
 *      Splicing the values into the text would show a statement the server
 *      never executed.
 *   3. EVERY BLOCK IS COPYABLE AND SCROLLS ON ITS OWN. A long statement scrolls
 *      inside its block, which is focusable so a keyboard can scroll it too, and
 *      the page never scrolls sideways.
 *
 * Shared by the Ask AI screen and the Insights detail, so the evidence for an
 * investigation and the evidence for an answer read the same way.
 */
import { useEffect, useRef, useState } from "react";

import type { SqlTrace, ToolTrace } from "../../lib/api";
import { Button, Disclosure } from "./ui";
import { IconCheck, IconClose, IconCopy } from "./icons";
import { num } from "../../lib/format";

/** "2 tool calls, 5 queries": what is inside the trail, before it is opened. */
export function traceSummary(traces: ToolTrace[]): string {
  const calls = traces.length;
  const reads = traces.reduce((n, t) => n + (t.queries?.length ?? 0), 0);
  return `${calls} tool ${calls === 1 ? "call" : "calls"}, ${reads} ${reads === 1 ? "query" : "queries"}`;
}

/** A Copy button that says "Copied" for a moment and then goes back. */
export function CopyButton({ value, label }: { value: string; label: string }) {
  const [copied, setCopied] = useState(false);
  const t = useRef<number | undefined>(undefined);
  useEffect(() => () => window.clearTimeout(t.current), []);
  return (
    <Button
      size="sm"
      tone="quiet"
      aria-label={copied ? `${label}: copied` : label}
      icon={copied ? <IconCheck size={13} /> : <IconCopy size={13} />}
      onClick={async () => {
        try {
          await navigator.clipboard.writeText(value);
          setCopied(true);
          window.clearTimeout(t.current);
          t.current = window.setTimeout(() => setCopied(false), 1600);
        } catch {
          /* The clipboard is blocked: the text is selectable, which is the fallback. */
        }
      }}
    >
      {copied ? "Copied" : "Copy"}
    </Button>
  );
}

/** One labelled block of code or data, scrolled inside itself, with a Copy button. */
function CodeBlock({ label, text, copy }: { label: string; text: string; copy?: string }) {
  return (
    <div className="mf-trail__block">
      <div className="mf-trail__block-head">
        <span className="mf-trail__label">{label}</span>
        <CopyButton value={copy ?? text} label={`Copy ${label.toLowerCase()}`} />
      </div>
      {/* Focusable, so a keyboard can scroll a statement wider than the panel. */}
      <pre className="mf-trail__code" tabIndex={0} role="region" aria-label={label}>
        {text}
      </pre>
    </div>
  );
}

/** A parameter as it would be written in SQL: strings quoted, numbers bare. */
const paramText = (p: string | number): string => (typeof p === "number" ? String(p) : JSON.stringify(p));

function Statement({ q, n }: { q: SqlTrace; n: number }) {
  const params = q.params ?? [];
  const copy = params.length
    ? `${q.sql}\n-- params, in order: ${params.map(paramText).join(", ")}`
    : q.sql;
  return (
    <div className="mf-trail__sql">
      <CodeBlock label={`SQL ${n}${q.report ? `: ${q.report}` : ""}`} text={q.sql} copy={copy} />
      <p className="mf-trail__params">
        <span className="mf-trail__label">Parameters, in order</span>{" "}
        {params.length ? (
          <span className="mf-data">{params.map(paramText).join(", ")}</span>
        ) : (
          <span className="mf-trail__quiet">none</span>
        )}
      </p>
    </div>
  );
}

function Call({ t, n }: { t: ToolTrace; n: number }) {
  const args = JSON.stringify(t.arguments ?? {}, null, 2);
  return (
    <li className="mf-trail__call">
      <div className="mf-trail__call-head">
        <span className="mf-trail__n mf-data" aria-hidden="true">
          {n}
        </span>
        <span className="mf-trail__tool mf-data">{t.tool}</span>
        {/* The outcome carries a glyph and a word, never a colour alone. */}
        <span className="mf-trail__state" data-ok={t.ok ? "true" : "false"}>
          {t.ok ? <IconCheck size={12} /> : <IconClose size={12} />}
          {t.ok ? "Ran" : "Failed"}
        </span>
        <span className="mf-trail__meta mf-data">
          {t.rows != null ? `${num(t.rows)} ${t.rows === 1 ? "row" : "rows"} · ` : ""}
          {num(t.took_ms)} ms
        </span>
      </div>
      {!t.ok && t.error ? <p className="mf-trail__error">{t.error}</p> : null}
      <CodeBlock label="Arguments" text={args} />
      {t.request ? <CodeBlock label="Equivalent stats request" text={t.request} /> : null}
      {(t.queries ?? []).map((q, i) => (
        <Statement key={i} q={q} n={i + 1} />
      ))}
      {!t.queries?.length ? <p className="mf-trail__quiet">This call ran no SQL.</p> : null}
    </li>
  );
}

export function QueryTrail({
  traces,
  summary = "The queries behind this answer",
  empty = "This answer ran no queries.",
  open,
}: {
  traces: ToolTrace[];
  summary?: string;
  /** Said instead of the disclosure when nothing was run. */
  empty?: string;
  open?: boolean;
}) {
  if (!traces.length) return <p className="mf-trail__none">{empty}</p>;
  return (
    <div className="mf-trail">
      <Disclosure
        open={open}
        summary={
          <span className="mf-trail__summary">
            {summary}
            <span className="mf-trail__count">{traceSummary(traces)}</span>
          </span>
        }
      >
        <ol className="mf-trail__list">
          {traces.map((t, i) => (
            <Call key={i} t={t} n={i + 1} />
          ))}
        </ol>
      </Disclosure>
    </div>
  );
}

export default QueryTrail;
