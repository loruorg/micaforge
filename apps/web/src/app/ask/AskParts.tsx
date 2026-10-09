/**
 * The pieces the Ask AI screen is drawn from: one message, the live list of
 * tools a question is running, the notice for a question that failed, the
 * conversation list, and the panel shown when no model is configured.
 *
 * They live apart from the screen so that the screen file holds only the one
 * hard part, sending a question and reading its answer as it arrives, and so
 * that each piece here is a plain function of what it is handed.
 *
 * Three rules hold for every piece:
 *
 *   1. AN ANSWER IS MARKED AS AI, AND ONLY AN ANSWER. The small "AI" badge
 *      (orange ink on white with a soft orange edge) is the one place orange
 *      appears on this screen. A question is a person's and stays neutral;
 *      no button, link or focus ring here is ever orange.
 *   2. EVERY ANSWER SHOWS ITS QUERIES. The trail of tool calls sits under each
 *      answer, failed answers included, because a failure that ran three
 *      queries first is still worth checking.
 *   3. A FAILURE SAYS WHAT HAPPENED AND WHAT TO DO. Not "an error occurred",
 *      but which of the six ways a question can fail this was, in words, with
 *      the one control that recovers it.
 */
import { useEffect, useRef, useState } from "react";
import { Link } from "react-router";
import { useMutation } from "@tanstack/react-query";
import type { UseQueryResult } from "@tanstack/react-query";

import { api } from "../../lib/api";
import type { AiConversationSummary, AiMessage, ToolTrace } from "../../lib/api";
import { ago, num } from "../../lib/format";
import { Button, Empty, ErrorState, Panel, PanelHead } from "../../components/react/ui";
import { IconAlert, IconCheck, IconClock, IconClose, IconOut, IconPlus } from "../../components/react/icons";
import { PlainText } from "../../components/react/PlainText";
import { QueryTrail } from "../../components/react/QueryTrail";
import { errText } from "../settings/SettingsLayout";

/** Where the set-up guide for this feature lives on the docs site. */
export const ASK_DOCS = "/docs/dashboard/ask-ai";

/* ── shapes the screen and these pieces share ────────────────────────────── */

/** One tool the model started, and its trace once it finished. */
export interface ToolStep {
  index: number;
  tool: string;
  trace: ToolTrace | null;
}

/**
 * The ways a question can fail, each with its own recovery:
 *
 *   not_configured  the server has no model           the operator sets two variables
 *   rate_limited    the daily question limit is hit   wait for midnight UTC
 *   network         the server was not reached        try again
 *   broken_early    the line dropped before the question was stored   try again
 *   stream_broken   the line dropped after it was stored   reload the conversation
 *   answer_error    the model was asked and did not answer   ask again
 *   other           the server refused the question for a stated reason
 */
export type FailureKind =
  | "not_configured"
  | "rate_limited"
  | "network"
  | "broken_early"
  | "stream_broken"
  | "answer_error"
  | "other";

export interface Failure {
  kind: FailureKind;
  message: string;
}

/* ── small marks ─────────────────────────────────────────────────────────── */

/** The AI badge. Text, not a colour alone: it reads "AI" in greyscale too. */
export function AiBadge() {
  return <span className="mf-ask-badge">AI</span>;
}

function When({ at }: { at: string }) {
  const t = Date.parse(at);
  return (
    <time className="mf-ask-msg__time" dateTime={at} title={Number.isFinite(t) ? new Date(t).toLocaleString() : undefined}>
      {ago(at)}
    </time>
  );
}

/* ── one message ─────────────────────────────────────────────────────────── */

/** A question, as the person typed it: line breaks kept, nothing interpreted. */
export function QuestionItem({ text, at }: { text: string; at: string }) {
  return (
    <li className="mf-ask-msg" data-role="user">
      <div className="mf-ask-msg__head">
        <span className="mf-ask-msg__who">You</span>
        <When at={at} />
      </div>
      <p className="mf-ask-msg__question">{text}</p>
    </li>
  );
}

/** One stored message: a question, or an answer with the queries behind it. */
export function MessageItem({ m }: { m: AiMessage }) {
  if (m.role === "user") return <QuestionItem text={m.content} at={m.created_at} />;
  return (
    <li className="mf-ask-msg" data-role="assistant">
      <div className="mf-ask-msg__head">
        <AiBadge />
        <span className="mf-ask-msg__who">Answer</span>
        <When at={m.created_at} />
      </div>
      {m.error ? (
        <div className="mf-ask-failed">
          <p className="mf-ask-failed__title">
            <IconAlert size={14} />
            The model did not answer
          </p>
          <p className="mf-ask-failed__body">{m.error}</p>
        </div>
      ) : null}
      {m.content.trim() ? <PlainText text={m.content} /> : null}
      {!m.error && !m.content.trim() ? (
        <p className="mf-ask-quiet">The model returned an empty answer.</p>
      ) : null}
      <QueryTrail traces={m.tool_calls ?? []} />
    </li>
  );
}

/* ── the tools a question is running, as they run ───────────────────────── */

export function Steps({ steps }: { steps: ToolStep[] }) {
  const allDone = steps.length > 0 && steps.every((s) => s.trace);
  return (
    <div className="mf-ask-steps" aria-live="polite">
      <p className="mf-ask-steps__title">
        <AiBadge />
        {steps.length ? "Working on the answer" : "Reading the question"}
      </p>
      {steps.length ? (
        <ol className="mf-ask-steps__list">
          {steps.map((s) => {
            const state = s.trace ? (s.trace.ok ? "ran" : "failed") : "running";
            return (
              <li key={s.index} className="mf-ask-step" data-state={state}>
                <span className="mf-ask-step__glyph" aria-hidden="true">
                  {state === "running" ? (
                    <IconClock size={13} />
                  ) : state === "ran" ? (
                    <IconCheck size={13} />
                  ) : (
                    <IconClose size={13} />
                  )}
                </span>
                {state === "running" ? (
                  <span>
                    Running <span className="mf-data">{s.tool}</span>…
                  </span>
                ) : state === "ran" ? (
                  <span>
                    Ran <span className="mf-data">{s.tool}</span>
                    <span className="mf-ask-step__meta mf-data">
                      {s.trace?.rows != null ? ` · ${num(s.trace.rows)} ${s.trace.rows === 1 ? "row" : "rows"}` : ""}
                      {` · ${num(s.trace?.took_ms)} ms`}
                    </span>
                  </span>
                ) : (
                  <span>
                    <span className="mf-data">{s.tool}</span> failed
                    {s.trace?.error ? `: ${s.trace.error}` : ""}
                  </span>
                )}
              </li>
            );
          })}
        </ol>
      ) : null}
      {allDone ? <p className="mf-ask-steps__next">Writing the answer…</p> : null}
    </div>
  );
}

/* ── a question that failed ──────────────────────────────────────────────── */

export function FailureNote({
  failure,
  dailyLimit,
  onRetry,
  onReload,
}: {
  failure: Failure;
  dailyLimit: number | null | undefined;
  onRetry: () => void;
  onReload: () => void;
}) {
  let title: string;
  let body: string;
  let action: "retry" | "reload" | "docs" | null = null;

  switch (failure.kind) {
    case "not_configured":
      title = "Ask AI is not set up on this instance";
      body =
        "The server has no language model to send the question to. Whoever runs this Micaforge instance sets MICAFORGE_LLM_BASE_URL and MICAFORGE_LLM_MODEL to turn it on. Your question is back in the box below.";
      action = "docs";
      break;
    case "rate_limited":
      title = "Daily question limit reached";
      body = `${
        dailyLimit
          ? `Your organisation has asked its ${num(dailyLimit)} questions for today.`
          : "Your organisation has reached its daily question limit."
      } The limit is shared by everyone in the organisation, including MCP clients and chat apps, and it resets at midnight UTC. Your question is back in the box below.`;
      break;
    case "network":
      title = "Could not reach the Micaforge server";
      body = "The question was not sent. Check that the server is running and reachable, then try again. Your question is back in the box below.";
      action = "retry";
      break;
    case "broken_early":
      title = "The connection closed before the question was stored";
      body = "Nothing was saved. Try again. Your question is back in the box below.";
      action = "retry";
      break;
    case "stream_broken":
      title = "The answer stopped arriving before it finished";
      body =
        "The question is stored, and the server keeps working on it after the connection drops, so the answer may already be saved. Reload this conversation in a moment to see it.";
      action = "reload";
      break;
    case "answer_error":
      title = "The model did not answer";
      body = failure.message || "The model returned an error and no answer. Ask again, or ask in other words.";
      break;
    case "other":
      title = "The question was not answered";
      body = `${failure.message} Your question is back in the box below.`;
      action = "retry";
      break;
  }

  return (
    <div className="mf-ask-note" role="alert">
      <p className="mf-ask-note__title">
        <IconAlert size={14} />
        {title}
      </p>
      <p className="mf-ask-note__body">{body}</p>
      {action === "retry" ? (
        <div className="mf-ask-note__act">
          <Button size="sm" onClick={onRetry}>
            Try again
          </Button>
        </div>
      ) : action === "reload" ? (
        <div className="mf-ask-note__act">
          <Button size="sm" onClick={onReload}>
            Reload this conversation
          </Button>
        </div>
      ) : action === "docs" ? (
        <div className="mf-ask-note__act">
          <a className="mf-ask-link" href={ASK_DOCS}>
            How to set up Ask AI
            <IconOut size={12} />
          </a>
        </div>
      ) : null}
    </div>
  );
}

/* ── no model configured ─────────────────────────────────────────────────── */

export function NotConfigured() {
  return (
    <Panel>
      <PanelHead
        title="Ask AI is not set up on this instance"
        note="Answers come from a language model that whoever runs this Micaforge instance connects. Until they do, no question is sent anywhere."
      />
      <div className="mf-ask-off">
        <p className="mf-ask-off__lead">To turn it on, the operator sets these variables and restarts the server:</p>
        <dl className="mf-ask-off__vars">
          <div className="mf-ask-off__var">
            <dt className="mf-data">MICAFORGE_LLM_BASE_URL</dt>
            <dd>
              The address of an OpenAI-compatible API, for example{" "}
              <span className="mf-data">https://api.openai.com/v1</span> or a local model server.
            </dd>
          </div>
          <div className="mf-ask-off__var">
            <dt className="mf-data">MICAFORGE_LLM_MODEL</dt>
            <dd>The name of the model to ask.</dd>
          </div>
          <div className="mf-ask-off__var">
            <dt className="mf-data">MICAFORGE_LLM_API_KEY</dt>
            <dd>Optional. The key, when the model API needs one.</dd>
          </div>
        </dl>
        <p className="mf-ask-off__lead">Conversations asked before are still listed below and can be read.</p>
        <a className="mf-ask-link" href={ASK_DOCS}>
          How to set up Ask AI
          <IconOut size={12} />
        </a>
      </div>
    </Panel>
  );
}

/* ── the conversation list ───────────────────────────────────────────────── */

function ConversationRow({
  c,
  active,
  base,
  onDeleted,
}: {
  c: AiConversationSummary;
  active: boolean;
  base: string;
  onDeleted: (id: string) => void;
}) {
  const [confirming, setConfirming] = useState(false);
  const keepRef = useRef<HTMLButtonElement | null>(null);
  const deleteRef = useRef<HTMLButtonElement | null>(null);
  const asked = useRef(false);
  const title = c.title.trim() || "Untitled conversation";

  const del = useMutation({
    mutationFn: () => api.del<{ deleted: string }>(`/api/ai/conversations/${encodeURIComponent(c.id)}`),
    onSuccess: () => onDeleted(c.id),
  });

  // The control that was clicked disappears, so focus goes to the control
  // that replaced it rather than falling back to the top of the page.
  useEffect(() => {
    if (!asked.current) return;
    if (confirming) keepRef.current?.focus();
    else deleteRef.current?.focus();
  }, [confirming]);

  return (
    <li className="mf-ask-conv" data-active={active ? "true" : "false"}>
      <div className="mf-ask-conv__row">
        <Link className="mf-ask-conv__link" to={`${base}/${c.id}`} aria-current={active ? "page" : undefined}>
          <span className="mf-ask-conv__title">{title}</span>
          <span className="mf-ask-conv__meta">
            {num(c.messages)} {c.messages === 1 ? "message" : "messages"} · {ago(c.updated_at)}
          </span>
        </Link>
        {confirming ? null : (
          <Button
            ref={deleteRef}
            size="sm"
            tone="quiet"
            className="mf-ask-conv__del"
            icon={<IconClose size={12} />}
            aria-label={`Delete the conversation "${title}"`}
            onClick={() => {
              asked.current = true;
              setConfirming(true);
            }}
          />
        )}
      </div>
      {confirming ? (
        <div className="mf-ask-conv__confirm" role="group" aria-label={`Delete the conversation "${title}"`}>
          <span className="mf-ask-conv__ask">Delete this conversation? This cannot be undone.</span>
          <span className="mf-ask-conv__keys">
            <Button size="sm" tone="danger" disabled={del.isPending} onClick={() => del.mutate()}>
              {del.isPending ? "Deleting…" : "Delete"}
            </Button>
            <Button ref={keepRef} size="sm" tone="quiet" disabled={del.isPending} onClick={() => setConfirming(false)}>
              Keep
            </Button>
          </span>
        </div>
      ) : null}
      {del.isError ? (
        <p className="mf-ask-conv__err" role="alert">
          {errText(del.error)}
        </p>
      ) : null}
    </li>
  );
}

export function ConversationList({
  list,
  base,
  openId,
  onNew,
  onDeleted,
}: {
  list: UseQueryResult<AiConversationSummary[]>;
  base: string;
  openId: string | null;
  onNew: () => void;
  onDeleted: (id: string) => void;
}) {
  return (
    <Panel className="mf-ask-list">
      <PanelHead
        title="Conversations"
        note="Yours only, newest first."
        actions={
          <Button size="sm" icon={<IconPlus size={13} />} onClick={onNew}>
            New
          </Button>
        }
      />
      {list.isPending ? (
        <div className="mf-ask-wait" aria-busy="true" aria-live="polite">
          <span className="mf-sr">Loading conversations…</span>
          {[72, 54, 64, 46].map((w, i) => (
            <span key={i} className="mf-wait" style={{ width: `${w}%` }} />
          ))}
        </div>
      ) : list.isError ? (
        <ErrorState
          title="Could not load your conversations"
          detail={errText(list.error)}
          onRetry={() => void list.refetch()}
        />
      ) : !list.data.length ? (
        <Empty
          title="No conversations yet"
          body="Each question you ask about this site starts or continues a conversation, and it is kept here. Nobody else can see it."
        />
      ) : (
        <ul className="mf-ask-convs">
          {list.data.map((c) => (
            <ConversationRow key={c.id} c={c} active={c.id === openId} base={base} onDeleted={onDeleted} />
          ))}
        </ul>
      )}
    </Panel>
  );
}

/* ── waiting for a conversation ──────────────────────────────────────────── */

export function ThreadWait() {
  return (
    <div className="mf-ask-wait mf-ask-wait--thread" aria-busy="true" aria-live="polite">
      <span className="mf-sr">Loading the conversation…</span>
      <span className="mf-wait" style={{ width: "38%" }} />
      <span className="mf-wait" style={{ width: "86%" }} />
      <span className="mf-wait" style={{ width: "74%" }} />
      <span className="mf-wait" style={{ width: "52%" }} />
    </div>
  );
}
