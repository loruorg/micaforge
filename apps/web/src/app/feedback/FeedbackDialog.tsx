/**
 * SEND FEEDBACK: the dialog every screen can open from the account menu.
 *
 * A report is only useful to the person who reads it if it says where the
 * problem was. Asking the reporter to paste the address of the page is asking
 * for the one detail they are least likely to think of, so the dialog sends it
 * itself, together with the site that is open, and SAYS so before anything is
 * sent: nothing leaves the browser that the reporter was not shown.
 *
 * Four decisions:
 *
 *   1. A DIALOG, NOT A SCREEN. A report is written about the screen that is
 *      open. A separate page would take the reporter away from the very thing
 *      they are describing, and would record the wrong address.
 *   2. RADIX FOR THE MECHANICS. The focus trap, Escape, the return of focus
 *      and the labelling come from @radix-ui/react-dialog, which already ships
 *      with the dashboard. Hand-written focus traps are where dialogs usually
 *      fail a keyboard user.
 *   3. THE FORM SAYS WHAT IS WRONG WITH ITSELF. A field the server refused is
 *      marked with the server's own sentence, against that field; anything that
 *      is not about one field is said once, under the form. Nothing is a toast.
 *   4. CLOSED MEANS CLEARED. Closing the dialog throws the draft away, so the
 *      next report never starts with half of the last one in it.
 */
import type { ReactNode, SubmitEvent } from "react";
import { useEffect, useId, useRef, useState } from "react";
import * as RDialog from "@radix-ui/react-dialog";
import { useMutation } from "@tanstack/react-query";

import { ApiError, api } from "../../lib/api";
import type { Feedback, FeedbackBody, FeedbackKind, FeedbackSeverity } from "../../lib/api";
import { Button } from "../../components/react/ui";
import { IconAlert, IconCheck, IconClose } from "../../components/react/icons";
import { num } from "../../lib/format";
import { useSite } from "../providers";
import { Select, TextArea, TextInput, errText } from "../settings/SettingsLayout";
import { KIND_WORDS, SEVERITY_WORDS } from "./words";
import "../../styles/app/feedback.css";

const TITLE_MIN = 3;
const TITLE_MAX = 140;
const BODY_MAX = 5000;

const KIND_HINT: Record<FeedbackKind, string> = {
  bug: "Something does not work the way it should.",
  problem: "Something works, but it is hard to use, slow or wrong for you.",
  feature: "Something Micaforge does not do yet.",
  other: "Anything else you want the people who run this instance to know.",
};

interface Draft {
  kind: FeedbackKind;
  title: string;
  body: string;
  severity: FeedbackSeverity | "";
}

const EMPTY: Draft = { kind: "bug", title: "", body: "", severity: "" };

type FieldName = "kind" | "title" | "body" | "severity";

/** What is wrong with the draft as written, field by field. */
function check(d: Draft): Partial<Record<FieldName, string>> {
  const out: Partial<Record<FieldName, string>> = {};
  if (d.title.trim().length < TITLE_MIN)
    out.title = `Write a title of at least ${TITLE_MIN} characters.`;
  if (!d.body.trim()) out.body = "Say what happened, or what you need.";
  return out;
}

/** The message for a refusal that is not about one field. */
function formError(e: unknown): string {
  if (e instanceof ApiError && e.status === 429)
    return "You have sent 20 reports in the last hour. Wait, then send again.";
  return errText(e);
}

/** One labelled control, with its hint or its error wired to it for a screen reader. */
function Row({
  id,
  label,
  hint,
  error,
  children,
}: {
  id: string;
  label: ReactNode;
  hint?: ReactNode;
  error?: string;
  children: ReactNode;
}) {
  return (
    <div className="mf-set-field">
      <label className="mf-set-field__label" htmlFor={id}>
        {label}
      </label>
      {children}
      {error ? (
        <p className="mf-fb-err" id={`${id}-note`}>
          <IconAlert size={13} />
          <span>{error}</span>
        </p>
      ) : hint ? (
        <p className="mf-set-field__hint" id={`${id}-note`}>
          {hint}
        </p>
      ) : null}
    </div>
  );
}

export function FeedbackDialog({
  open,
  onOpenChange,
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
}) {
  const { siteId, domain } = useSite();
  const base = useId();
  const ids = {
    kind: `${base}-kind`,
    title: `${base}-title`,
    body: `${base}-body`,
    severity: `${base}-severity`,
  };

  const [draft, setDraft] = useState<Draft>(EMPTY);
  const [tried, setTried] = useState(false);
  const doneRef = useRef<HTMLDivElement | null>(null);
  /* The shared settings fields take no ref, so a field is found by its id. */
  const focusField = (f: FieldName) => document.getElementById(ids[f])?.focus();

  const send = useMutation({
    mutationFn: (body: FeedbackBody) => api.post<Feedback>("/api/feedback", body).then((r) => r.data),
  });

  // Closed means cleared, however the dialog was closed.
  const { reset } = send;
  useEffect(() => {
    if (open) return;
    setDraft(EMPTY);
    setTried(false);
    reset();
  }, [open, reset]);

  // The thanks replaces the form, so focus goes to it rather than to nowhere.
  useEffect(() => {
    if (send.isSuccess) doneRef.current?.focus();
  }, [send.isSuccess]);

  const pageUrl = open && typeof window !== "undefined" ? window.location.href : "";

  const local = tried ? check(draft) : {};
  const serverField =
    send.error instanceof ApiError && send.error.field ? (send.error.field as FieldName) : null;
  const errorFor = (f: FieldName): string | undefined =>
    local[f] ?? (serverField === f ? (send.error as ApiError).message : undefined);
  const generalError = send.isError && !serverField ? formError(send.error) : null;

  const set = (patch: Partial<Draft>) => {
    setDraft((d) => ({ ...d, ...patch }));
    if (send.isError) reset();
  };

  const submit = (e: SubmitEvent<HTMLFormElement>) => {
    e.preventDefault();
    setTried(true);
    const problems = check(draft);
    if (problems.title) {
      focusField("title");
      return;
    }
    if (problems.body) {
      focusField("body");
      return;
    }
    send.mutate({
      kind: draft.kind,
      title: draft.title.trim(),
      body: draft.body.trim(),
      ...(draft.severity ? { severity: draft.severity } : {}),
      ...(siteId != null ? { site: siteId } : {}),
      page_url: window.location.href,
    });
  };

  const describe = (f: FieldName, hasHint: boolean) =>
    errorFor(f) || hasHint ? `${ids[f]}-note` : undefined;

  return (
    <RDialog.Root open={open} onOpenChange={onOpenChange}>
      <RDialog.Portal>
        <RDialog.Overlay className="mf-fb-overlay" />
        <RDialog.Content
          className="mf-fb-dialog"
          onOpenAutoFocus={(e) => {
            // The first field, not the close button in the corner.
            e.preventDefault();
            focusField("kind");
          }}
        >
          <div className="mf-fb-dialog__head">
            <RDialog.Title className="mf-fb-dialog__title">Send feedback</RDialog.Title>
            <RDialog.Close asChild>
              <button type="button" className="mf-fb-dialog__x" aria-label="Close">
                <IconClose size={14} />
              </button>
            </RDialog.Close>
          </div>
          <RDialog.Description className="mf-fb-dialog__note">
            Report a bug or a problem, or ask for a feature. It goes to the people who run this
            Micaforge instance.
          </RDialog.Description>

          {send.isSuccess ? (
            <div className="mf-fb-done" ref={doneRef} tabIndex={-1} role="status">
              <p className="mf-fb-done__head">
                <IconCheck size={15} />
                <span>Thank you. Your report is filed.</span>
              </p>
              <p className="mf-fb-done__body">
                The administrators of this instance see it in their feedback list, with the address
                of the page you were on.
                {send.data?.id ? (
                  <>
                    {" "}
                    Its reference is <span className="mf-data">{send.data.id}</span>.
                  </>
                ) : null}
              </p>
              <div className="mf-fb-dialog__foot">
                <Button
                  onClick={() => {
                    setDraft(EMPTY);
                    setTried(false);
                    reset();
                  }}
                >
                  Send another
                </Button>
                <RDialog.Close asChild>
                  <Button tone="primary">Close</Button>
                </RDialog.Close>
              </div>
            </div>
          ) : (
            <form className="mf-fb-form" onSubmit={submit} noValidate>
              <div className="mf-fb-pair">
                <Row id={ids.kind} label="Kind" hint={KIND_HINT[draft.kind]} error={errorFor("kind")}>
                  <Select
                    id={ids.kind}
                    value={draft.kind}
                    aria-invalid={errorFor("kind") ? true : undefined}
                    aria-describedby={describe("kind", true)}
                    onChange={(e) => set({ kind: e.target.value as FeedbackKind })}
                  >
                    {(Object.keys(KIND_WORDS) as FeedbackKind[]).map((k) => (
                      <option key={k} value={k}>
                        {KIND_WORDS[k]}
                      </option>
                    ))}
                  </Select>
                </Row>
                <Row
                  id={ids.severity}
                  label="How serious is it (optional)"
                  error={errorFor("severity")}
                >
                  <Select
                    id={ids.severity}
                    value={draft.severity}
                    aria-invalid={errorFor("severity") ? true : undefined}
                    aria-describedby={describe("severity", false)}
                    onChange={(e) => set({ severity: e.target.value as FeedbackSeverity | "" })}
                  >
                    <option value="">Not set</option>
                    {(Object.keys(SEVERITY_WORDS) as FeedbackSeverity[]).map((s) => (
                      <option key={s} value={s}>
                        {SEVERITY_WORDS[s]}
                      </option>
                    ))}
                  </Select>
                </Row>
              </div>

              <Row
                id={ids.title}
                label="Title"
                hint={`${num(draft.title.length)} of ${TITLE_MAX} characters.`}
                error={errorFor("title")}
              >
                <TextInput
                  id={ids.title}
                  value={draft.title}
                  maxLength={TITLE_MAX}
                  autoComplete="off"
                  aria-invalid={errorFor("title") ? true : undefined}
                  aria-describedby={describe("title", true)}
                  onChange={(e) => set({ title: e.target.value })}
                />
              </Row>

              <Row
                id={ids.body}
                label="Details"
                hint={`What you did, what you expected, and what happened instead. ${num(draft.body.length)} of ${num(BODY_MAX)} characters.`}
                error={errorFor("body")}
              >
                <TextArea
                  id={ids.body}
                  className="mf-fb-area"
                  rows={6}
                  value={draft.body}
                  maxLength={BODY_MAX}
                  aria-invalid={errorFor("body") ? true : undefined}
                  aria-describedby={describe("body", true)}
                  onChange={(e) => set({ body: e.target.value })}
                />
              </Row>

              <p className="mf-fb-page">
                <span>
                  The address of this page{domain ? <> and the site you have open ({domain})</> : null}{" "}
                  is sent with the report:
                </span>
                <span className="mf-fb-page__url mf-data" title={pageUrl}>
                  {pageUrl}
                </span>
              </p>

              {generalError ? (
                <p className="mf-fb-err mf-fb-err--form" role="alert">
                  <IconAlert size={14} />
                  <span>{generalError}</span>
                </p>
              ) : null}

              <div className="mf-fb-dialog__foot">
                <RDialog.Close asChild>
                  <Button tone="quiet">Cancel</Button>
                </RDialog.Close>
                <Button tone="primary" type="submit" disabled={send.isPending}>
                  {send.isPending ? "Sending…" : "Send feedback"}
                </Button>
              </div>
            </form>
          )}
        </RDialog.Content>
      </RDialog.Portal>
    </RDialog.Root>
  );
}

export default FeedbackDialog;
