/**
 * One short link, opened: its addresses, its QR code, and the way to change or
 * remove it.
 *
 * The address people are sent to is printed as the server built it
 * (`final_url`), not rebuilt here: after a save, the server's version is the
 * truth, and an operator checking a campaign link needs to see exactly what a
 * click will open.
 *
 * The QR code is an SVG the server draws. It is fetched as a blob and shown
 * through an object URL in an <img>, never written into the page as markup, so
 * nothing inside the file can run. The same blob is what "Download SVG" saves,
 * so the file on disk is the one on screen.
 *
 * Deleting asks once, in place, and says what deleting means: the short
 * address stops working, and printed QR codes with it.
 */
import type { ReactNode } from "react";
import { useEffect, useRef, useState } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";

import { Button, CopyField, Panel, PanelHead } from "../../components/react/ui";
import { IconClose, IconDownload, IconPen } from "../../components/react/icons";
import { API_BASE, ApiError, api } from "../../lib/api";
import { hostOf, num } from "../../lib/format";
import { Say } from "./controls";
import { day, moment, problem } from "./model";
import type { Folder, Link } from "./model";

/* ── the QR code ─────────────────────────────────────────────────────────── */

async function fetchQr(id: string): Promise<Blob> {
  let res: Response;
  try {
    res = await fetch(`${API_BASE}/api/links/${encodeURIComponent(id)}/qr.svg?size=512`, {
      credentials: "include",
    });
  } catch {
    throw new ApiError(0, "network", "Could not reach the Micaforge server.");
  }
  if (!res.ok) throw new ApiError(res.status, "qr", `The server could not draw the QR code (status ${res.status}).`);
  // Typed explicitly: an <img> only renders an SVG blob that says it is one.
  return new Blob([await res.text()], { type: "image/svg+xml" });
}

function QrCode({ link }: { link: Link }) {
  const qr = useQuery({
    queryKey: ["links", "qr", link.id, link.short_url],
    queryFn: () => fetchQr(link.id),
    staleTime: 10 * 60_000,
  });
  const [src, setSrc] = useState<string | null>(null);

  useEffect(() => {
    if (!qr.data) {
      setSrc(null);
      return;
    }
    const u = URL.createObjectURL(qr.data);
    setSrc(u);
    return () => URL.revokeObjectURL(u);
  }, [qr.data]);

  const download = () => {
    if (!qr.data) return;
    const u = URL.createObjectURL(qr.data);
    const a = document.createElement("a");
    a.href = u;
    a.download = `${link.slug}.svg`;
    document.body.appendChild(a);
    a.click();
    a.remove();
    window.setTimeout(() => URL.revokeObjectURL(u), 1000);
  };

  return (
    <section className="mf-gr-qr" aria-label="QR code">
      <div className="mf-gr-qr__frame">
        {qr.isPending ? (
          <span className="mf-gr-qr__wait mf-wait" aria-busy="true">
            <span className="mf-sr">Loading the QR code…</span>
          </span>
        ) : qr.isError ? (
          <div className="mf-gr-qr__err">
            <Say tone="warn">Could not load the QR code. {problem(qr.error)}</Say>
            <Button size="sm" onClick={() => void qr.refetch()}>
              Try again
            </Button>
          </div>
        ) : src ? (
          <img className="mf-gr-qr__img" src={src} alt={`QR code for ${link.short_url}`} width={176} height={176} />
        ) : null}
      </div>
      <div className="mf-gr-qr__side">
        <p className="mf-gr-qr__text">
          Scanning it opens <span className="mf-data">{link.short_url.replace(/^https?:\/\//, "")}</span>, so
          scans are counted like any other click. The SVG scales to any print size.
        </p>
        <Button size="sm" icon={<IconDownload size={13} />} disabled={!qr.data} onClick={download}>
          Download SVG
        </Button>
      </div>
    </section>
  );
}

/* ── the link panel ──────────────────────────────────────────────────────── */

function Fact({ name, children }: { name: string; children: ReactNode }) {
  return (
    <div className="mf-gr-facts__row">
      <dt className="mf-gr-facts__name">{name}</dt>
      <dd className="mf-gr-facts__value">{children}</dd>
    </div>
  );
}

export function LinkDetail({
  link,
  folders,
  onEdit,
  onClose,
  onDeleted,
}: {
  link: Link;
  folders: Folder[];
  onEdit: () => void;
  onClose: () => void;
  onDeleted: () => void;
}) {
  const qc = useQueryClient();
  const [asking, setAsking] = useState(false);
  const headRef = useRef<HTMLDivElement>(null);

  // A newly opened link takes focus, so a keyboard reader lands on what they
  // just chose rather than back at the top of the list.
  useEffect(() => {
    headRef.current?.focus({ preventScroll: false });
    setAsking(false);
  }, [link.id]);

  const remove = useMutation({
    mutationFn: () => api.del<{ deleted: boolean }>(`/api/links/${encodeURIComponent(link.id)}`),
    onSuccess: () => {
      void qc.invalidateQueries({ queryKey: ["links"] });
      onDeleted();
    },
  });

  const folder = link.folder_id ? folders.find((f) => f.id === link.folder_id)?.name ?? "A deleted folder" : "No folder";

  return (
    <Panel className="mf-gr-detail">
      <div ref={headRef} tabIndex={-1} className="mf-gr-detail__focus">
        <PanelHead
          title={link.title?.trim() || `/${link.slug}`}
          note={link.expired ? "This link has expired. Clicks now see an expired page." : `Goes to ${hostOf(link.target_url)}`}
          actions={
            <>
              <Button size="sm" icon={<IconPen size={13} />} onClick={onEdit}>
                Edit
              </Button>
              <Button size="sm" tone="quiet" icon={<IconClose size={13} />} onClick={onClose}>
                Close
              </Button>
            </>
          }
        />
      </div>

      <div className="mf-gr-pad">
        <CopyField label="Short link" value={link.short_url} />

        <dl className="mf-gr-facts">
          <Fact name="Opens">
            <span className="mf-gr-facts__url mf-data">{link.final_url}</span>
          </Fact>
          {link.ios_url ? (
            <Fact name="On iOS">
              <span className="mf-gr-facts__url mf-data">{link.ios_url}</span>
            </Fact>
          ) : null}
          {link.android_url ? (
            <Fact name="On Android">
              <span className="mf-gr-facts__url mf-data">{link.android_url}</span>
            </Fact>
          ) : null}
          {link.deep_link ? (
            <Fact name="App link">
              <span className="mf-gr-facts__url mf-data">{link.deep_link}</span>
            </Fact>
          ) : null}
          <Fact name="Folder">{folder}</Fact>
          <Fact name="Clicks, last 30 days">
            <span className="mf-data">{num(link.clicks_30d)}</span> by people
          </Fact>
          <Fact name="Expires">
            {link.expired ? (
              <span className="mf-gr-state" data-tone="off">
                Expired {link.expires_at ? `on ${moment(link.expires_at)}` : ""}
              </span>
            ) : link.expires_at ? (
              moment(link.expires_at)
            ) : (
              "Never"
            )}
          </Fact>
          <Fact name="Created">{day(link.created_at)}</Fact>
        </dl>

        <QrCode link={link} />

        <div className="mf-gr-detail__danger">
          {asking ? (
            <div className="mf-gr-ask" role="group" aria-label="Confirm deleting this link">
              <p className="mf-gr-ask__text">
                Delete <span className="mf-data">/{link.slug}</span>? The short address stops working at once,
                and so do printed QR codes for it.
              </p>
              <div className="mf-gr-ask__acts">
                <Button tone="danger" size="sm" disabled={remove.isPending} onClick={() => remove.mutate()}>
                  {remove.isPending ? "Deleting…" : "Delete link"}
                </Button>
                <Button size="sm" onClick={() => setAsking(false)}>
                  Keep it
                </Button>
              </div>
            </div>
          ) : (
            <Button tone="danger" size="sm" onClick={() => setAsking(true)}>
              Delete link
            </Button>
          )}
          {remove.isError ? <Say tone="warn">{problem(remove.error)}</Say> : null}
        </div>
      </div>
    </Panel>
  );
}
