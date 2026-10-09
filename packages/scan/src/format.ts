/**
 * A report as text: a table for a terminal, JSON for a program, Markdown for a pull
 * request comment or a tracking-plan page.
 *
 * All three say the same things in the same order: the events with their properties and
 * where they are sent from, the computed names, the findings with errors first, and the
 * comparison with the site when there was one.
 */
import type { EventSummary, Finding, Occurrence, ScanReport } from "./model.js";

/** The output formats. */
export type Format = "table" | "json" | "markdown";

/** Colours for a terminal, or nothing. */
interface Paint {
  bold: (s: string) => string;
  dim: (s: string) => string;
  red: (s: string) => string;
  yellow: (s: string) => string;
  cyan: (s: string) => string;
}

const plain: Paint = { bold: (s) => s, dim: (s) => s, red: (s) => s, yellow: (s) => s, cyan: (s) => s };
const ansi: Paint = {
  bold: (s) => `\u001b[1m${s}\u001b[22m`,
  dim: (s) => `\u001b[2m${s}\u001b[22m`,
  red: (s) => `\u001b[31m${s}\u001b[39m`,
  yellow: (s) => `\u001b[33m${s}\u001b[39m`,
  cyan: (s) => `\u001b[36m${s}\u001b[39m`,
};

/** `file:line`. */
const at = (o: { file?: string; line?: number }) => (o.file ? `${o.file}${o.line ? `:${o.line}` : ""}` : "");

/** Properties as `plan: string, seats?: number`. */
function propsText(event: EventSummary): string {
  const parts = event.props.map((p) => `${p.key}${p.everywhere ? "" : "?"}: ${p.types.join(" | ")}`);
  if (event.propsDynamic) parts.push("…");
  return parts.join(", ");
}

/** Where an event is sent from: the first three places and how many more. */
function placesText(occurrences: Occurrence[]): string {
  const first = occurrences.slice(0, 3).map(at).join(", ");
  return occurrences.length > 3 ? `${first} and ${occurrences.length - 3} more` : first;
}

/** Counts of findings by severity. */
export function tally(findings: Finding[]): { errors: number; warnings: number; infos: number } {
  return {
    errors: findings.filter((f) => f.severity === "error").length,
    warnings: findings.filter((f) => f.severity === "warning").length,
    infos: findings.filter((f) => f.severity === "info").length,
  };
}

/** Pad to a width, by characters. */
const pad = (s: string, width: number) => (s.length >= width ? s : s + " ".repeat(width - s.length));

/** The terminal table. */
export function formatTable(report: ScanReport, color: boolean): string {
  const p = color ? ansi : plain;
  const lines: string[] = [];
  const counts = tally(report.findings);
  lines.push(
    p.bold(
      `${report.events.length} event${report.events.length === 1 ? "" : "s"} in ${report.files} file${report.files === 1 ? "" : "s"}`,
    ) + (report.dynamic.length ? p.dim(`, and ${report.dynamic.length} computed name${report.dynamic.length === 1 ? "" : "s"}`) : ""),
  );
  lines.push("");

  if (report.events.length) {
    const nameWidth = Math.min(40, Math.max(5, ...report.events.map((e) => e.name.length)));
    lines.push(p.dim(`  ${pad("EVENT", nameWidth)}  SENT  PROPERTIES`));
    for (const event of report.events) {
      const label = event.name || '""';
      const shown = label.length > nameWidth ? `${label.slice(0, nameWidth - 1)}…` : label;
      lines.push(`  ${p.cyan(pad(shown, nameWidth))}  ${pad(String(event.occurrences.length), 4)}  ${propsText(event)}`);
      lines.push(p.dim(`  ${pad("", nameWidth)}        ${placesText(event.occurrences)}`));
    }
    lines.push("");
  }

  if (report.dynamic.length) {
    lines.push(p.bold("Computed names"));
    for (const o of report.dynamic) {
      lines.push(`  ${o.pattern ?? o.expression ?? "?"}  ${p.dim(at(o))}`);
    }
    lines.push("");
  }

  if (report.comparison) {
    const c = report.comparison;
    lines.push(p.bold(`Compared with site ${c.site} over ${c.range}`));
    lines.push(`  ${c.received.length} event name${c.received.length === 1 ? "" : "s"} received`);
    if (c.notReceived.length) lines.push(`  In the code, never received: ${c.notReceived.join(", ")}`);
    if (c.notInCode.length) {
      lines.push(`  Received, not in the code: ${c.notInCode.map((r) => `${r.name} (${r.count})`).join(", ")}`);
    }
    if (c.truncated) lines.push(p.dim("  The site has more names than one read returns; the list above is partial."));
    lines.push("");
  }

  const order = { error: 0, warning: 1, info: 2 } as const;
  const shown = [...report.findings].sort((a, b) => order[a.severity] - order[b.severity]);
  for (const f of shown) {
    const label =
      f.severity === "error" ? p.red("error  ") : f.severity === "warning" ? p.yellow("warning") : p.dim("info   ");
    lines.push(`${label} ${f.message}${f.file ? p.dim(`  ${at(f)}`) : ""}`);
  }
  if (shown.length) lines.push("");
  lines.push(
    `${counts.errors ? p.red(`${counts.errors} error${counts.errors === 1 ? "" : "s"}`) : "0 errors"}, ${
      counts.warnings ? p.yellow(`${counts.warnings} warning${counts.warnings === 1 ? "" : "s"}`) : "0 warnings"
    }`,
  );
  return `${lines.join("\n")}\n`;
}

/** Escape a Markdown table cell. */
const cell = (s: string) => s.replace(/\|/g, "\\|").replace(/\n/g, " ");

/** The Markdown report. */
export function formatMarkdown(report: ScanReport): string {
  const out: string[] = [];
  const counts = tally(report.findings);
  out.push("# Analytics events");
  out.push("");
  out.push(
    `${report.events.length} events in ${report.files} files. ${counts.errors} errors, ${counts.warnings} warnings.`,
  );
  out.push("");
  if (report.events.length) {
    out.push("| Event | Sent from | Properties |");
    out.push("| --- | --- | --- |");
    for (const e of report.events) {
      out.push(`| \`${cell(e.name)}\` | ${cell(placesText(e.occurrences))} | ${cell(propsText(e)) || "none"} |`);
    }
    out.push("");
  }
  if (report.dynamic.length) {
    out.push("## Computed names");
    out.push("");
    for (const o of report.dynamic) out.push(`- \`${cell(o.pattern ?? o.expression ?? "?")}\` at ${at(o)}`);
    out.push("");
  }
  if (report.comparison) {
    const c = report.comparison;
    out.push(`## Compared with site ${c.site} (${c.range})`);
    out.push("");
    out.push(`- Received: ${c.received.length} event names`);
    out.push(`- In the code, never received: ${c.notReceived.map((n) => `\`${n}\``).join(", ") || "none"}`);
    out.push(
      `- Received, not in the code: ${c.notInCode.map((r) => `\`${r.name}\` (${r.count})`).join(", ") || "none"}`,
    );
    if (c.truncated) out.push("- The site has more names than one read returns; this list is partial.");
    out.push("");
  }
  if (report.findings.length) {
    out.push("## Findings");
    out.push("");
    out.push("| Severity | Check | Message | Where |");
    out.push("| --- | --- | --- | --- |");
    for (const f of report.findings) out.push(`| ${f.severity} | ${f.check} | ${cell(f.message)} | ${cell(at(f))} |`);
    out.push("");
  }
  return out.join("\n");
}

/** Render a report in one of the formats. */
export function formatReport(report: ScanReport, format: Format, color = false): string {
  if (format === "json") return `${JSON.stringify(report, null, 2)}\n`;
  if (format === "markdown") return formatMarkdown(report);
  return formatTable(report, color);
}
