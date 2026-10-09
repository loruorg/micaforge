/**
 * Ask AI, at /app/:site/ask and /app/:site/ask/:conversation
 *
 * A question about this site, in plain words, answered from this site's own
 * data. The model does not see the database; it calls the same reports every
 * other screen reads (overview, breakdowns, AI crawlers, the Citation gap,
 * goals, funnels), each one bounded to this site, and every call it made is
 * listed under the answer with the stats request and the SQL behind it. An
 * answer that cannot show its working is a guess, and this screen does not
 * print guesses.
 *
 * Four decisions are worth knowing before changing anything here:
 *
 *  1. THE QUESTION CARRIES THE DASHBOARD'S SCOPE. The range, the audience and
 *     the filters set in the top bar travel with every question, exactly as
 *     they travel with every report read, and the line above the box says what
 *     they are. "Which pages lost visitors?" means the window the reader is
 *     already looking at, not one the model chose.
 *  2. THE ANSWER ARRIVES AS IT IS MADE. A question can run several tools, and
 *     a box that says nothing for twenty seconds reads as a hang. The answer is
 *     read as a stream: each tool is shown when it starts and when it ends, then
 *     the answer. A browser or a proxy that cannot stream gets the same answer
 *     as one JSON reply, and loses nothing but the progress lines.
 *  3. A STREAMED ANSWER IS NEVER OVERWRITTEN BY A SLOWER READ. The in-flight
 *     question is kept on this screen and drawn after the stored conversation,
 *     matched by message id, so a refetch that lands before the answer is
 *     stored cannot make the answer vanish, and one that lands after it cannot
 *     draw it twice.
 *  4. NOTHING IS SENT WHEN NO MODEL IS CONFIGURED. The status is read first.
 *     Without a model the screen says, in so many words, which two variables
 *     the operator sets, and offers no box to type into. Past conversations
 *     are still listed and can still be read.
 */
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { Link, useNavigate, useParams } from "react-router";
import { useQuery, useQueryClient } from "@tanstack/react-query";

import { ApiError, api, canStream, postEventStream } from "../../lib/api";
import type {
  AiConversation,
  AiConversationSummary,
  AiMessage,
  AiStatus,
  AskAnswer,
  AskBody,
  ToolTrace,
} from "../../lib/api";
import { ago, num } from "../../lib/format";
import { Button, Empty, ErrorState, Panel, PanelHead } from "../../components/react/ui";
import { rangeWords, useAppState } from "../state";
import { useSite } from "../providers";
import { errText } from "../settings/SettingsLayout";
import {
  AiBadge,
  ConversationList,
  FailureNote,
  MessageItem,
  NotConfigured,
  QuestionItem,
  Steps,
  ThreadWait,
} from "./AskParts";
import type { Failure, ToolStep } from "./AskParts";
import "../../styles/app/ask.css";

/** The server refuses a longer question, so the box stops at the same place. */
const MAX_QUESTION = 2000;

const SUGGESTIONS = [
  "Which pages lost the most visitors this week?",
  "How many visits came from AI answers in the last 30 days, and from which tools?",
  "Which AI crawlers fetched the most pages, and are they verified?",
  "Which sources bring visitors who convert on my goals?",
  "What changed in traffic compared to the previous period?",
];

const AUDIENCE_WORDS = { human: "People", agent: "AI crawlers", all: "All" } as const;

const STATUS_KEY = ["ai", "status"] as const;
const listKey = (siteId: number | undefined) => ["ai", "conversations", siteId ?? null] as const;
const threadKey = (id: string | null) => ["ai", "conversation", id] as const;

/* ── one question in flight ──────────────────────────────────────────────── */

type Phase = "sending" | "streaming" | "done" | "failed";

interface Turn {
  question: string;
  /** When it was sent, for the question's own timestamp until the server's arrives. */
  sentAt: string;
  /** The conversation that was open when it was sent; `null` for a new one. */
  startedIn: string | null;
  /** The conversation it belongs to: the open one, or the one the server starts. */
  conversationId: string | null;
  userMessage: AiMessage | null;
  tools: ToolStep[];
  answer: AiMessage | null;
  phase: Phase;
  failure: Failure | null;
}

interface ConversationEvent {
  conversation_id: string;
  user_message: AiMessage;
}
interface ToolStartEvent {
  index: number;
  tool: string;
}
interface ToolEndEvent {
  index: number;
  trace: ToolTrace;
}

const isObject = (v: unknown): v is Record<string, unknown> => typeof v === "object" && v !== null;

function browserZone(): string | undefined {
  try {
    return Intl.DateTimeFormat().resolvedOptions().timeZone;
  } catch {
    return undefined;
  }
}

/**
 * The question and the dashboard's current scope, as the server wants them.
 * Read from the store at the moment of sending, so a range changed while the
 * box was being typed in is the range the question is asked about.
 */
function askBody(siteId: number, question: string, conversationId: string | null, tz: string | undefined): AskBody {
  const env = useAppState.getState().toEnvelope(siteId);
  const body: AskBody = { site: siteId, question };
  if (conversationId) body.conversation_id = conversationId;
  if (env.range) body.range = env.range;
  if (env.start) body.start = env.start;
  if (env.end) body.end = env.end;
  if (env.audience) body.audience = env.audience;
  if (env.filters?.length) body.filters = env.filters;
  const zone = tz ?? browserZone();
  if (zone) body.tz = zone;
  return body;
}

/** A refusal or a lost line, named as one of the ways a question can fail. */
function failureFrom(e: unknown, stored: boolean): Failure {
  if (e instanceof ApiError) {
    if (e.code === "ai_not_configured") return { kind: "not_configured", message: e.message };
    if (e.status === 429 || e.code === "rate_limited") return { kind: "rate_limited", message: e.message };
    if (e.code === "network") return { kind: "network", message: e.message };
    if (e.code === "stream_broken") return { kind: stored ? "stream_broken" : "broken_early", message: e.message };
  }
  return { kind: "other", message: errText(e) };
}

/** The `error` event that ends a stream, named the same way. */
function failureFromEvent(code: string, message: string): Failure {
  if (code === "ai_not_configured") return { kind: "not_configured", message };
  if (code === "rate_limited") return { kind: "rate_limited", message };
  return { kind: "answer_error", message };
}

/**
 * True once the stored conversation holds everything this question produced:
 * its answer by id, or any answer stored after its question. From then on the
 * stored copy is drawn and the in-flight one is not.
 */
function settledInStore(t: Turn, stored: AiMessage[]): boolean {
  const answer = t.answer;
  if (answer && stored.some((m) => m.id === answer.id)) return true;
  const asked = t.userMessage;
  if (!asked) return false;
  const at = stored.findIndex((m) => m.id === asked.id);
  return at >= 0 && stored.slice(at + 1).some((m) => m.role === "assistant");
}

const prefersReducedMotion = () =>
  typeof window !== "undefined" && window.matchMedia?.("(prefers-reduced-motion: reduce)").matches === true;

/* ── the screen ──────────────────────────────────────────────────────────── */

export function AskScreen() {
  const { site, conversation } = useParams();
  const conversationId = conversation ?? null;
  const navigate = useNavigate();
  const qc = useQueryClient();
  const { siteId, domain, timezone } = useSite();
  const range = useAppState((s) => s.range);
  const audience = useAppState((s) => s.audience);
  const filterCount = useAppState((s) => s.filters.length);
  const base = `/app/${site ?? ""}/ask`;

  const status = useQuery({
    queryKey: STATUS_KEY,
    queryFn: () => api.get<AiStatus>("/api/ai/status").then((r) => r.data),
    staleTime: 60_000,
  });

  const list = useQuery({
    queryKey: listKey(siteId),
    queryFn: () =>
      api
        .get<AiConversationSummary[]>(`/api/ai/conversations?site=${encodeURIComponent(String(siteId))}`)
        .then((r) => r.data),
    enabled: siteId != null,
  });

  const thread = useQuery({
    queryKey: threadKey(conversationId),
    queryFn: () =>
      api
        .get<AiConversation>(`/api/ai/conversations/${encodeURIComponent(conversationId ?? "")}`)
        .then((r) => r.data),
    enabled: conversationId != null,
  });

  const [draft, setDraft] = useState("");
  const [turn, setTurnState] = useState<Turn | null>(null);
  const turnRef = useRef<Turn | null>(null);
  const abortRef = useRef<AbortController | null>(null);
  const endRef = useRef<HTMLDivElement | null>(null);
  const inputRef = useRef<HTMLTextAreaElement | null>(null);

  /** The ref is read by the stream callbacks, which outlive any one render. */
  const setTurn = useCallback((t: Turn | null) => {
    turnRef.current = t;
    setTurnState(t);
  }, []);

  const busy = turn != null && (turn.phase === "sending" || turn.phase === "streaming");
  const configured = status.data?.configured === true;

  /* A question belongs to one conversation. Opening another one, or starting
     a new one, stops reading the old answer; the server still stores it. The
     move from /ask to /ask/<id> that the first event causes is not a change of
     conversation: the turn knows both addresses, the one it was sent from and
     the one the server named, so it holds whichever of the two is open. */
  useEffect(() => {
    const t = turnRef.current;
    if (t && t.conversationId !== conversationId && t.startedIn !== conversationId) {
      abortRef.current?.abort();
      abortRef.current = null;
      setTurn(null);
    }
  }, [conversationId, setTurn]);

  useEffect(() => () => abortRef.current?.abort(), []);

  const send = useCallback(
    async (text: string) => {
      const question = text.trim();
      if (!question || question.length > MAX_QUESTION || siteId == null) return;
      if (turnRef.current && (turnRef.current.phase === "sending" || turnRef.current.phase === "streaming")) return;

      abortRef.current?.abort();
      const ctrl = new AbortController();
      abortRef.current = ctrl;

      const startedIn = conversationId;
      setTurn({
        question,
        sentAt: new Date().toISOString(),
        startedIn,
        conversationId: startedIn,
        userMessage: null,
        tools: [],
        answer: null,
        phase: "sending",
        failure: null,
      });
      setDraft("");

      /** Applies a change to the turn, unless this question has been abandoned. */
      const update = (fn: (t: Turn) => Turn) => {
        const current = turnRef.current;
        if (abortRef.current !== ctrl || !current) return;
        setTurn(fn(current));
      };

      /** The server named the conversation: open it, keeping this question. */
      const adopt = (id: string) => {
        update((t) => ({ ...t, conversationId: id }));
        if (startedIn !== id) {
          navigate(`${base}/${id}`, { replace: startedIn == null });
          void qc.invalidateQueries({ queryKey: ["ai", "conversations"] });
        }
      };

      const finish = (answer: AiMessage) => update((t) => ({ ...t, answer, phase: "done" }));

      const body = askBody(siteId, question, startedIn, timezone);

      try {
        if (canStream()) {
          await postEventStream(
            "/api/ai/ask",
            body,
            (ev) => {
              const d = ev.data;
              if (!isObject(d)) return;
              switch (ev.event) {
                case "conversation": {
                  const c = d as unknown as ConversationEvent;
                  if (typeof c.conversation_id !== "string") return;
                  adopt(c.conversation_id);
                  update((t) => ({ ...t, userMessage: c.user_message ?? null, phase: "streaming" }));
                  return;
                }
                case "tool_start": {
                  const s = d as unknown as ToolStartEvent;
                  update((t) => ({
                    ...t,
                    phase: "streaming",
                    tools: [...t.tools.filter((x) => x.index !== s.index), { index: s.index, tool: s.tool, trace: null }].sort(
                      (a, b) => a.index - b.index,
                    ),
                  }));
                  return;
                }
                case "tool_end": {
                  const s = d as unknown as ToolEndEvent;
                  if (!isObject(s.trace)) return;
                  update((t) => ({
                    ...t,
                    tools: t.tools.some((x) => x.index === s.index)
                      ? t.tools.map((x) => (x.index === s.index ? { ...x, trace: s.trace } : x))
                      : [...t.tools, { index: s.index, tool: s.trace.tool, trace: s.trace }].sort((a, b) => a.index - b.index),
                  }));
                  return;
                }
                case "answer": {
                  const m = (d as { message?: AiMessage }).message;
                  if (m) finish(m);
                  return;
                }
                case "error": {
                  const e = d as { code?: string; message?: string };
                  const failure = failureFromEvent(e.code ?? "error", e.message ?? "");
                  update((t) => ({ ...t, phase: "failed", failure }));
                  // Refused before it was stored: the question goes back in the box.
                  if (!turnRef.current?.userMessage) setDraft((v) => (v.trim() ? v : question));
                  if (failure.kind === "not_configured") void qc.invalidateQueries({ queryKey: STATUS_KEY });
                  return;
                }
                case "json": {
                  const a = d as unknown as AskAnswer;
                  if (typeof a.conversation_id === "string") adopt(a.conversation_id);
                  if (a.message) finish(a.message);
                  return;
                }
              }
            },
            ctrl.signal,
          );
          // The line closed without an answer or an error: the stream broke.
          const t = turnRef.current;
          if (abortRef.current === ctrl && t && (t.phase === "sending" || t.phase === "streaming")) {
            throw new ApiError(0, "stream_broken", "The answer stream stopped before it finished.");
          }
        } else {
          const r = await api.post<AskAnswer>("/api/ai/ask", body);
          adopt(r.data.conversation_id);
          finish(r.data.message);
        }
      } catch (e) {
        if (ctrl.signal.aborted || abortRef.current !== ctrl) return;
        const stored = turnRef.current?.userMessage != null;
        const failure = failureFrom(e, stored);
        update((t) => ({ ...t, phase: "failed", failure }));
        // A question the server never stored goes back in the box, so nothing
        // the reader typed is lost to a refusal or a dropped line.
        if (!stored) setDraft((d) => (d.trim() ? d : question));
        if (failure.kind === "not_configured") void qc.invalidateQueries({ queryKey: STATUS_KEY });
      } finally {
        if (abortRef.current === ctrl) {
          abortRef.current = null;
          void qc.invalidateQueries({ queryKey: ["ai", "conversations"] });
          const id = turnRef.current?.conversationId;
          if (id) void qc.invalidateQueries({ queryKey: threadKey(id) });
        }
      }
    },
    [base, conversationId, navigate, qc, setTurn, siteId, timezone],
  );

  const startNew = useCallback(() => {
    abortRef.current?.abort();
    abortRef.current = null;
    setTurn(null);
    if (conversationId) navigate(base);
    inputRef.current?.focus();
  }, [base, conversationId, navigate, setTurn]);

  const onDeleted = useCallback(
    (id: string) => {
      void qc.invalidateQueries({ queryKey: ["ai", "conversations"] });
      qc.removeQueries({ queryKey: threadKey(id) });
      if (id === conversationId) navigate(base, { replace: true });
    },
    [base, conversationId, navigate, qc],
  );

  /* ── what the thread shows ─────────────────────────────────────────────── */

  const stored: AiMessage[] = useMemo(
    () => (conversationId && thread.data?.id === conversationId ? thread.data.messages : []),
    [conversationId, thread.data],
  );
  const storedIds = useMemo(() => new Set(stored.map((m) => m.id)), [stored]);
  const live =
    turn &&
    (turn.conversationId === conversationId || turn.startedIn === conversationId) &&
    !settledInStore(turn, stored)
      ? turn
      : null;

  /* The question is drawn while it is on its way and once it is stored. A
     question the server refused is not drawn: it is back in the box. */
  const showQuestion =
    live != null &&
    (live.userMessage
      ? !storedIds.has(live.userMessage.id)
      : live.answer != null || live.phase === "sending" || live.phase === "streaming");

  const progress = live
    ? `${live.phase}|${live.tools.length}|${live.tools.filter((t) => t.trace).length}|${live.answer?.id ?? ""}|${live.failure?.kind ?? ""}`
    : "";
  useEffect(() => {
    if (!progress) return;
    endRef.current?.scrollIntoView({ block: "nearest", behavior: prefersReducedMotion() ? "auto" : "smooth" });
  }, [progress]);

  const threadTitle =
    (conversationId && thread.data?.id === conversationId && thread.data.title.trim()) ||
    (live ? live.question : "") ||
    "New conversation";

  const threadNote =
    conversationId && thread.data?.id === conversationId
      ? `Started ${ago(thread.data.created_at)}. Only you can see this conversation.`
      : "Saved to your conversations when the first question is sent. Only you can see it.";

  const notFound = thread.error instanceof ApiError && thread.error.status === 404;
  const threadLoading = conversationId != null && thread.isPending && !live;
  const threadFailed = conversationId != null && thread.isError && !live;
  const threadEmpty = !threadLoading && !threadFailed && stored.length === 0 && !live;

  const scope = [
    rangeWords(range),
    AUDIENCE_WORDS[audience],
    filterCount ? `${filterCount} ${filterCount === 1 ? "filter" : "filters"}` : "no filters",
  ].join(" · ");

  const limit = status.data?.daily_limit;
  const used = draft.length;
  const canSend = configured && !busy && siteId != null && draft.trim().length > 0 && used <= MAX_QUESTION;

  /* ── drawing ───────────────────────────────────────────────────────────── */

  return (
    <div className="mf-ask">
      <header className="mf-ask-bar">
        <div className="mf-ask-bar__text">
          <h1 className="mf-ask-bar__title">Ask AI</h1>
          <p className="mf-ask-bar__note">
            Questions about {domain ?? "this site"}, answered from this site&apos;s own data. Every answer lists the
            queries it ran, so each figure can be checked.
          </p>
        </div>
        {configured ? (
          <p className="mf-ask-bar__model">
            <AiBadge />
            <span>
              Model <span className="mf-data">{status.data?.model ?? "not named"}</span>
            </span>
            <span className="mf-ask-bar__sep" aria-hidden="true">
              ·
            </span>
            <span>
              {limit
                ? `${num(limit)} questions a day, shared by your organisation`
                : "No daily question limit"}
            </span>
          </p>
        ) : null}
      </header>

      {status.data && !status.data.configured ? <NotConfigured /> : null}

      <div className="mf-ask-cols">
        <ConversationList list={list} base={base} openId={conversationId} onNew={startNew} onDeleted={onDeleted} />

        <Panel className="mf-ask-main">
          <PanelHead title={<span className="mf-ask-main__title">{threadTitle}</span>} note={threadNote} />

          <div className="mf-ask-body">
            {threadLoading ? (
              <ThreadWait />
            ) : threadFailed ? (
              notFound ? (
                <Empty
                  title="This conversation is not here"
                  body="It was deleted, or it belongs to someone else. Conversations are private to the person who asked."
                  action={
                    <Link className="mf-btn mf-btn--key" to={base}>
                      <span className="mf-btn__label">Start a new conversation</span>
                    </Link>
                  }
                />
              ) : (
                <ErrorState
                  title="Could not load this conversation"
                  detail={errText(thread.error)}
                  onRetry={() => void thread.refetch()}
                />
              )
            ) : threadEmpty ? (
              configured ? (
                <div className="mf-ask-start">
                  <h2 className="mf-ask-start__title">Ask a question about {domain ?? "this site"}</h2>
                  <p className="mf-ask-start__body">
                    The answer is read from this site&apos;s data for the range, audience and filters set in the top
                    bar. Start with one of these, or write your own below.
                  </p>
                  <ul className="mf-ask-suggest">
                    {SUGGESTIONS.map((s) => (
                      <li key={s}>
                        <button
                          type="button"
                          className="mf-ask-suggest__q"
                          disabled={busy || siteId == null}
                          onClick={() => void send(s)}
                        >
                          {s}
                        </button>
                      </li>
                    ))}
                  </ul>
                </div>
              ) : status.isPending ? (
                <ThreadWait />
              ) : (
                <Empty
                  title="No conversation open"
                  body={
                    status.isError
                      ? "Choose a conversation from the list to read it."
                      : "No new question can be sent until a model is set up. Choose a past conversation from the list to read it."
                  }
                />
              )
            ) : (
              <ol className="mf-ask-thread">
                {stored.map((m) => (
                  <MessageItem key={m.id} m={m} />
                ))}
                {live && showQuestion ? (
                  <QuestionItem
                    text={live.userMessage?.content ?? live.question}
                    at={live.userMessage?.created_at ?? live.sentAt}
                  />
                ) : null}
                {live && (live.phase === "sending" || live.phase === "streaming") ? (
                  <li className="mf-ask-msg" data-role="assistant">
                    <Steps steps={live.tools} />
                  </li>
                ) : null}
                {live?.answer ? <MessageItem m={live.answer} /> : null}
                {live?.failure ? (
                  <li className="mf-ask-msg" data-role="notice">
                    <FailureNote
                      failure={live.failure}
                      dailyLimit={limit}
                      onRetry={() => void send(draft.trim() || live.question)}
                      onReload={() => void thread.refetch()}
                    />
                  </li>
                ) : null}
              </ol>
            )}
            <div ref={endRef} />
          </div>

          {status.isPending ? (
            <div className="mf-ask-compose">
              <div className="mf-ask-wait" aria-busy="true" aria-live="polite">
                <span className="mf-sr">Checking whether Ask AI is set up…</span>
                <span className="mf-wait" style={{ width: "100%", height: "64px" }} />
              </div>
            </div>
          ) : status.isError ? (
            <div className="mf-ask-compose">
              <ErrorState
                title="Could not check whether Ask AI is set up"
                detail={errText(status.error)}
                onRetry={() => void status.refetch()}
              />
            </div>
          ) : configured ? (
            <form
              className="mf-ask-compose"
              aria-busy={busy}
              onSubmit={(e) => {
                e.preventDefault();
                if (canSend) void send(draft);
              }}
            >
              <label className="mf-sr" htmlFor="mf-ask-question">
                Your question
              </label>
              <p className="mf-ask-compose__scope">
                Asked about <span className="mf-ask-compose__scope-v">{scope}</span>. Change these in the top bar.
              </p>
              <textarea
                id="mf-ask-question"
                ref={inputRef}
                className="mf-ask-compose__box"
                value={draft}
                rows={3}
                maxLength={MAX_QUESTION}
                readOnly={busy}
                aria-describedby="mf-ask-question-hint"
                placeholder="Ask about this site's traffic, pages, sources or AI crawlers"
                onChange={(e) => setDraft(e.target.value)}
                onKeyDown={(e) => {
                  if (e.key === "Enter" && !e.shiftKey && !e.nativeEvent.isComposing) {
                    e.preventDefault();
                    if (canSend) void send(draft);
                  }
                }}
              />
              <div className="mf-ask-compose__foot">
                <span id="mf-ask-question-hint" className="mf-ask-compose__hint">
                  Enter sends. Shift and Enter start a new line.
                </span>
                <span className="mf-ask-compose__count mf-data">
                  {num(used)} / {num(MAX_QUESTION)}
                </span>
                <Button type="submit" tone="primary" size="sm" disabled={!canSend}>
                  {busy ? "Answering…" : "Send"}
                </Button>
              </div>
            </form>
          ) : null}
        </Panel>
      </div>
    </div>
  );
}

export default AskScreen;
