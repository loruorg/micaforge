/**
 * CHAT APPS: Slack, Discord and Telegram, for one site.
 *
 * A chat app is two halves owned by two different people, and this screen
 * keeps them apart on purpose:
 *
 *   1. THE APP ITSELF belongs to whoever runs this instance. It exists only
 *      when its variables are set on the server, and a developer console on
 *      the platform's side has to be told where to send its requests. So the
 *      first panel states, per platform, whether the app is configured, and
 *      gives the exact addresses to paste, each with a Copy button. Nothing on
 *      this screen can switch an app on; it says which variables would.
 *   2. THE LINK belongs to the site. A channel is tied to a site by typing a
 *      one-time code in it, so nobody can point a stranger's channel at this
 *      site's numbers, and nobody can read them from a channel nobody linked.
 *      The code is generated here, works once, and lasts fifteen minutes; the
 *      exact command for each platform is shown beside it.
 *
 * Once a channel is linked, anyone in it can ask questions about this site and
 * new investigations are posted there.
 */
import { useEffect, useState } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";

import {
  Button,
  CopyField,
  Empty,
  Panel,
  PanelHead,
  Sheet,
  type Column,
} from "../../components/react/ui";
import { IconClose, IconOut } from "../../components/react/icons";
import { api } from "../../lib/api";
import type { ChatLink, ChatLinkCode, ChatPlatform, ChatStatus } from "../../lib/api";
import { ago } from "../../lib/format";
import { useSite } from "../providers";
import { Block, Caution, Foot, Note, QueryState, Status, errText } from "./SettingsLayout";
import "../../styles/app/connect.css";

const PLATFORM_NAME: Record<ChatPlatform, string> = {
  slack: "Slack",
  discord: "Discord",
  telegram: "Telegram",
};

const PLATFORMS: ChatPlatform[] = ["slack", "discord", "telegram"];

/** The variables that switch each app on. All of a group, or none of it. */
const PLATFORM_VARS: Record<ChatPlatform, string[]> = {
  slack: ["MICAFORGE_SLACK_SIGNING_SECRET", "MICAFORGE_SLACK_BOT_TOKEN"],
  discord: ["MICAFORGE_DISCORD_APPLICATION_ID", "MICAFORGE_DISCORD_PUBLIC_KEY", "MICAFORGE_DISCORD_BOT_TOKEN"],
  telegram: ["MICAFORGE_TELEGRAM_BOT_TOKEN", "MICAFORGE_TELEGRAM_WEBHOOK_SECRET"],
};

/** A guide link, set as an ink link with an outward arrow; never blue or orange. */
function Guide({ to, children }: { to: string; children: string }) {
  return (
    <a className="mf-cx-link" href={to}>
      {children}
      <IconOut size={12} />
    </a>
  );
}

/** The variable names, in the data face, joined as a sentence. */
function VarList({ names }: { names: string[] }) {
  return (
    <>
      {names.map((n, i) => (
        <span key={n}>
          {i > 0 ? (i === names.length - 1 ? " and " : ", ") : null}
          <code className="mf-data">{n}</code>
        </span>
      ))}
    </>
  );
}

/* ── the apps on this instance ───────────────────────────────────────────── */

function PlatformBlock({ platform, status }: { platform: ChatPlatform; status: ChatStatus }) {
  const name = PLATFORM_NAME[platform];
  const on = status[platform].configured;

  const head = (
    <span className="mf-cx-plat__head">
      {name}
      <Status state={on ? "done" : "waiting"} label={on ? "Configured" : "Not configured"} />
    </span>
  );

  return (
    <Block
      title={head}
      body={
        on ? undefined : (
          <>
            The operator of this instance switches it on by setting <VarList names={PLATFORM_VARS[platform]} />{" "}
            on the server and restarting it.
          </>
        )
      }
    >
      {platform === "slack" ? (
        <>
          <CopyField label="Slash command /micaforge, request URL" value={status.slack.commands_url} />
          <CopyField label="Event Subscriptions, request URL" value={status.slack.events_url} />
          <Note>
            Subscribe the app to the <code className="mf-data">app_mention</code> and{" "}
            <code className="mf-data">message.im</code> events, with the bot scopes{" "}
            <code className="mf-data">commands</code>, <code className="mf-data">chat:write</code>,{" "}
            <code className="mf-data">app_mentions:read</code> and <code className="mf-data">im:history</code>.
          </Note>
        </>
      ) : null}
      {platform === "discord" ? (
        <>
          <CopyField label="Interactions endpoint URL" value={status.discord.interactions_url} />
          {status.discord.application_id ? (
            <Note>
              Application id <span className="mf-data">{status.discord.application_id}</span>. An instance
              administrator registers the <code className="mf-data">/micaforge</code> command once with{" "}
              <code className="mf-data">micaforge-server chat-setup discord</code>.
            </Note>
          ) : null}
        </>
      ) : null}
      {platform === "telegram" ? (
        <>
          <CopyField label="Webhook URL" value={status.telegram.webhook_url} />
          <Note>
            The server sets this webhook itself, with its secret token: an instance administrator runs{" "}
            <code className="mf-data">micaforge-server chat-setup telegram</code> once.
          </Note>
        </>
      ) : null}
      <div>
        <Guide to={`/docs/integrations/${platform}`}>{`Set up ${name}`}</Guide>
      </div>
    </Block>
  );
}

/* ── a one-time link code ────────────────────────────────────────────────── */

/** Minutes left until `iso`, recounted every fifteen seconds while it matters. */
function useMinutesLeft(iso: string | null): number | null {
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    if (!iso) return;
    const t = window.setInterval(() => setNow(Date.now()), 15_000);
    return () => window.clearInterval(t);
  }, [iso]);
  if (!iso) return null;
  const at = Date.parse(iso);
  return Number.isFinite(at) ? Math.max(0, Math.ceil((at - now) / 60_000)) : null;
}

function LinkCode({ issued, status }: { issued: ChatLinkCode; status: ChatStatus }) {
  const left = useMinutesLeft(issued.expires_at);
  const expired = left === 0;
  const at = new Date(issued.expires_at).toLocaleTimeString(undefined, { hour: "2-digit", minute: "2-digit" });
  const usable = PLATFORMS.filter((p) => status[p].configured);

  return (
    <div className="mf-cx-code">
      <p className="mf-cx-code__value mf-data" aria-label={`Link code ${issued.code.split("").join(" ")}`}>
        {issued.code}
      </p>
      <p className="mf-cx-code__left" role="status">
        {expired
          ? "This code has expired. Generate a new one."
          : `Works once. Expires at ${at}, in ${left} ${left === 1 ? "minute" : "minutes"}.`}
      </p>
      {expired ? null : (
        <div className="mf-cx-code__cmds">
          {usable.map((p) => (
            <CopyField key={p} label={`Type this in the ${PLATFORM_NAME[p]} channel`} value={issued.commands[p]} />
          ))}
        </div>
      )}
    </div>
  );
}

/* ── the screen ──────────────────────────────────────────────────────────── */

export function ChatAppsSettings() {
  const { siteId, domain } = useSite();
  const qc = useQueryClient();
  const linksKey = ["settings", "chat-links", siteId];

  const status = useQuery({
    queryKey: ["chat", "status"],
    queryFn: () => api.get<ChatStatus>("/api/chat/status").then((r) => r.data),
  });

  const links = useQuery({
    queryKey: linksKey,
    queryFn: () => api.get<ChatLink[]>("/api/chat/links", { site: siteId as number }).then((r) => r.data),
    enabled: siteId != null,
  });

  const [issued, setIssued] = useState<ChatLinkCode | null>(null);

  const generate = useMutation({
    mutationFn: () =>
      api
        .post<ChatLinkCode>(`/api/chat/link-codes?site=${encodeURIComponent(String(siteId))}`)
        .then((r) => r.data),
    onSuccess: (code) => setIssued(code),
  });

  const unlink = useMutation({
    mutationFn: (id: string) => api.del<{ deleted: string }>(`/api/chat/links/${id}`),
    onSuccess: () => qc.invalidateQueries({ queryKey: linksKey }),
  });

  const anyOn = status.data ? PLATFORMS.some((p) => status.data[p].configured) : false;

  const columns: Column<ChatLink>[] = [
    {
      key: "where",
      head: "Channel",
      render: (l) => (
        <span>
          {PLATFORM_NAME[l.platform] ?? l.platform} · {l.label || "unnamed channel"}
        </span>
      ),
    },
    {
      key: "ids",
      head: "Workspace and channel",
      width: "26%",
      render: (l) => (
        <span className="mf-data">
          {l.workspace_id ? `${l.workspace_id} / ` : ""}
          {l.channel_id}
        </span>
      ),
    },
    {
      key: "by",
      head: "Linked by",
      width: "16%",
      render: (l) => <span>{l.created_by ?? "unknown"}</span>,
    },
    {
      key: "when",
      head: "Linked",
      width: "14%",
      render: (l) => (
        <span className="mf-data" title={new Date(l.created_at).toLocaleString()}>
          {ago(l.created_at)}
        </span>
      ),
    },
    {
      key: "act",
      head: <span className="mf-sr">Actions</span>,
      numeric: true,
      width: "1%",
      render: (l) => (
        <Button
          size="sm"
          tone="quiet"
          icon={<IconClose size={12} />}
          aria-label={`Unlink ${PLATFORM_NAME[l.platform] ?? l.platform} channel ${l.label}`}
          disabled={unlink.isPending}
          onClick={() => unlink.mutate(l.id)}
        >
          Unlink
        </Button>
      ),
    },
  ];

  return (
    <>
      <Panel>
        <PanelHead
          title="Chat apps on this instance"
          note="Ask questions about this site and get new investigations from Slack, Discord or Telegram. Each app is switched on by the operator of this instance."
        />
        <QueryState query={status}>
          {(s) => (
            <>
              {PLATFORMS.map((p) => (
                <PlatformBlock key={p} platform={p} status={s} />
              ))}
              {s.ai_configured ? null : (
                <Foot>
                  <Note>
                    Questions in chat need the AI analyst, which is not configured on this instance. The{" "}
                    <code className="mf-data">stats</code>, <code className="mf-data">link</code>,{" "}
                    <code className="mf-data">feedback</code> and <code className="mf-data">help</code> commands
                    work without it.
                  </Note>
                </Foot>
              )}
            </>
          )}
        </QueryState>
      </Panel>

      <Panel>
        <PanelHead
          title="Linked channels"
          note={`Channels that can ask about ${domain ?? "this site"} and receive its investigations.`}
        />
        <QueryState query={links}>
          {(rows) => (
            <>
              <Sheet
                rows={rows}
                columns={columns}
                getKey={(l) => l.id}
                empty={
                  <Empty
                    title="No channel is linked yet"
                    body="Generate a link code below and type the command in the channel you want to link."
                  />
                }
              />
              {unlink.isError ? (
                <Foot tight>
                  <span className="mf-set-foot__say" data-tone="warn" role="alert">
                    {errText(unlink.error)}
                  </span>
                </Foot>
              ) : null}
            </>
          )}
        </QueryState>
      </Panel>

      <Panel>
        <PanelHead
          title="Link a channel"
          note="A link code ties one channel to this site. It works once and expires after fifteen minutes."
        />
        <Block
          title="Generate a link code"
          body="Then type the command shown for your platform in the channel. The bot answers there when the link is made."
        >
          {status.data && !anyOn ? (
            <Caution>No chat app is configured on this instance, so a link code has nowhere to be used yet.</Caution>
          ) : null}
          <div className="mf-set-row">
            <Button
              tone="primary"
              disabled={siteId == null || generate.isPending || (status.data != null && !anyOn)}
              onClick={() => generate.mutate()}
            >
              {generate.isPending ? "Generating…" : issued ? "Generate a new code" : "Generate a link code"}
            </Button>
            {generate.isError ? (
              <span className="mf-set-foot__say" data-tone="warn" role="alert">
                {errText(generate.error)}
              </span>
            ) : null}
          </div>
          {issued && status.data ? <LinkCode issued={issued} status={status.data} /> : null}
        </Block>
      </Panel>
    </>
  );
}

export default ChatAppsSettings;
