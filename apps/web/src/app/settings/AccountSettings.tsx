/**
 * ACCOUNT: who is reading the record.
 *
 * The two-factor and session panels are written the way the rest of this
 * product writes about privacy: state exactly what is stored and what it can
 * do, then give the control.
 *
 * There is no password panel because there is no password: every sign-in is a
 * code sent to the account's address. Two-factor adds an authenticator code on
 * top of it, and turning two-factor off asks for one, so a session left open on
 * a shared machine cannot remove it.
 */
import { useState } from "react";
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
import { IconCheck, IconClose } from "../../components/react/icons";
import { api } from "../../lib/api";
import { ago } from "../../lib/format";
import {
  Block,
  Caution,
  Field,
  Foot,
  QueryState,
  Result,
  Spec,
  SpecRow,
  Status,
  TextInput,
  TypedConfirm,
  errText,
} from "./SettingsLayout";

interface Me {
  id: string;
  email: string;
  name: string;
  avatar_url: string | null;
  email_verified_at: string | null;
  totp_enabled: boolean;
  created_at: string;
}

interface AuthSession {
  id: string;
  user_agent: string;
  ip_hash: string;
  expires_at: string;
  created_at?: string;
  current?: boolean;
}

interface TotpStart {
  secret: string;
  otpauth_url?: string;
}

export function AccountSettings() {
  const qc = useQueryClient();

  const me = useQuery({
    queryKey: ["account", "me"],
    queryFn: () => api.get<Me>("/api/auth/me").then((r) => r.data),
  });

  /* profile ------------------------------------------------------------- */
  const [name, setName] = useState<string | null>(null);
  const [avatar, setAvatar] = useState<string | null>(null);
  const profile = {
    name: name ?? me.data?.name ?? "",
    avatar_url: avatar ?? me.data?.avatar_url ?? "",
  };
  const profileDirty =
    !!me.data &&
    (profile.name !== me.data.name || profile.avatar_url !== (me.data.avatar_url ?? ""));

  const saveProfile = useMutation({
    mutationFn: () => api.patch<Me>("/api/auth/me", profile),
    onSuccess: (r) => {
      qc.setQueryData(["account", "me"], r.data);
      setName(null);
      setAvatar(null);
    },
  });

  /* two-factor ---------------------------------------------------------- */
  const [enrolling, setEnrolling] = useState<TotpStart | null>(null);
  const [code, setCode] = useState("");
  const [offCode, setOffCode] = useState("");

  const startTotp = useMutation({
    mutationFn: () => api.post<TotpStart>("/api/auth/totp/enroll").then((r) => r.data),
    onSuccess: (d) => setEnrolling(d),
  });
  const enableTotp = useMutation({
    mutationFn: () => api.post<null>("/api/auth/totp/verify", { code }),
    onSuccess: () => {
      setEnrolling(null);
      setCode("");
      void qc.invalidateQueries({ queryKey: ["account", "me"] });
    },
  });
  const disableTotp = useMutation({
    mutationFn: () => api.post<null>("/api/auth/totp/disable", { code: offCode }),
    onSuccess: () => {
      setOffCode("");
      void qc.invalidateQueries({ queryKey: ["account", "me"] });
    },
    onError: () => setOffCode(""),
  });

  /* sessions ------------------------------------------------------------ */
  const sessions = useQuery({
    queryKey: ["account", "sessions"],
    queryFn: () => api.get<AuthSession[]>("/api/auth/sessions").then((r) => r.data),
  });
  const revoke = useMutation({
    mutationFn: (id: string) => api.del<null>(`/api/auth/sessions/${id}`),
    onSuccess: () => qc.invalidateQueries({ queryKey: ["account", "sessions"] }),
  });
  const revokeOthers = useMutation({
    mutationFn: () => api.post<null>("/api/auth/sessions/revoke-others"),
    onSuccess: () => qc.invalidateQueries({ queryKey: ["account", "sessions"] }),
  });

  const deleteAccount = useMutation({
    mutationFn: () => api.del<null>("/api/auth/me"),
    onSuccess: () => {
      window.location.href = "/";
    },
  });

  const sessionColumns: Column<AuthSession>[] = [
    {
      key: "ua",
      head: "Client",
      render: (s) => (
        <span className="mf-data" title={s.user_agent}>
          {s.user_agent || "unknown client"}
        </span>
      ),
    },
    {
      key: "ip",
      head: "Address hash",
      width: "18%",
      render: (s) => <span className="mf-data">{(s.ip_hash || "").slice(0, 12) || "—"}</span>,
    },
    {
      key: "exp",
      head: "Expires",
      width: "16%",
      render: (s) => <span className="mf-data">{ago(s.expires_at)}</span>,
    },
    {
      key: "act",
      head: <span className="mf-sr">Actions</span>,
      numeric: true,
      width: "1%",
      render: (s) =>
        s.current ? (
          <Status state="running" label="This one" />
        ) : (
          <Button
            size="sm"
            tone="quiet"
            icon={<IconClose size={12} />}
            disabled={revoke.isPending}
            onClick={() => revoke.mutate(s.id)}
          >
            Sign out
          </Button>
        ),
    },
  ];

  return (
    <>
      <Panel>
        <PanelHead title="Profile" note="Your name appears on invitations and in the members list of every organisation you belong to." />
        <QueryState query={me}>
          {(u) => (
            <>
              <Spec>
                <SpecRow name="Name" htmlFor="acc-name">
                  <TextInput id="acc-name" value={profile.name} onChange={(e) => setName(e.target.value)} />
                </SpecRow>
                <SpecRow
                  name="Email"
                  hint="Your sign-in codes and your reports go to this address. It is the account: there is no password beside it."
                  htmlFor="acc-email"
                >
                  <TextInput id="acc-email" type="email" data value={u.email} readOnly />
                  <div className="mf-set-row">
                    {u.email_verified_at ? (
                      <Status state="done" label={`Verified ${ago(u.email_verified_at)}`} />
                    ) : (
                      <Status state="waiting" label="Confirmed at your next sign-in" />
                    )}
                  </div>
                </SpecRow>
                <SpecRow
                  name="Avatar"
                  hint="A URL to an image you host. Micaforge does not fetch or store the picture itself."
                  htmlFor="acc-avatar"
                >
                  <TextInput
                    id="acc-avatar"
                    data
                    spellCheck={false}
                    placeholder="https://example.com/me.jpg"
                    value={profile.avatar_url}
                    onChange={(e) => setAvatar(e.target.value)}
                  />
                </SpecRow>
              </Spec>
              <Foot>
                <Button
                  tone="primary"
                  icon={<IconCheck size={14} />}
                  disabled={!profileDirty || saveProfile.isPending}
                  onClick={() => saveProfile.mutate()}
                >
                  Save profile
                </Button>
                <Button
                  tone="quiet"
                  disabled={!profileDirty}
                  onClick={() => {
                    setName(null);
                    setAvatar(null);
                  }}
                >
                  Discard
                </Button>
                <span className="mf-set-foot__spacer" />
                <Result action={saveProfile} done="Profile saved." />
              </Foot>
            </>
          )}
        </QueryState>
      </Panel>

      <Panel>
        <PanelHead
          title="Two-factor"
          note="A time-based code from an authenticator app, asked for after the code sent to your email. The secret is sealed at rest and never leaves the server after enrolment."
        />
        <QueryState query={me}>
          {(u) =>
            u.totp_enabled ? (
              <Block
                title="Two-factor is on"
                body="Turning it off needs a code from the authenticator, so a session left open somewhere cannot do it."
              >
                <div className="mf-set-row">
                  <Field label="Authenticator code" htmlFor="totp-off">
                    <TextInput
                      id="totp-off"
                      data
                      inputMode="numeric"
                      autoComplete="one-time-code"
                      pattern="[0-9]*"
                      maxLength={6}
                      value={offCode}
                      onChange={(e) => setOffCode(e.target.value.replace(/\D/g, "").slice(0, 6))}
                    />
                  </Field>
                  <Button
                    tone="danger"
                    disabled={offCode.length !== 6 || disableTotp.isPending}
                    onClick={() => disableTotp.mutate()}
                  >
                    Turn two-factor off
                  </Button>
                </div>
                <Result action={disableTotp} done="Two-factor is off." working="Turning off…" />
              </Block>
            ) : enrolling ? (
              <Block
                title="Add the secret to your authenticator"
                body="Paste the secret into any TOTP app, then type the six digits it shows to finish."
              >
                <CopyField label="Secret" value={enrolling.secret} />
                {enrolling.otpauth_url ? (
                  <CopyField label="Or the full otpauth URL" value={enrolling.otpauth_url} />
                ) : null}
                <div className="mf-set-row">
                  <Field label="The six digits" htmlFor="totp-code">
                    <TextInput
                      id="totp-code"
                      data
                      inputMode="numeric"
                      autoComplete="one-time-code"
                      maxLength={6}
                      value={code}
                      onChange={(e) => setCode(e.target.value.replace(/\D/g, ""))}
                    />
                  </Field>
                  <Button
                    tone="primary"
                    disabled={code.length !== 6 || enableTotp.isPending}
                    onClick={() => enableTotp.mutate()}
                  >
                    Finish
                  </Button>
                  <Button tone="quiet" onClick={() => setEnrolling(null)}>
                    Cancel
                  </Button>
                </div>
                <Caution>
                  From then on every sign-in asks for a code from this authenticator, and so does
                  turning two-factor off. If it is lost, the operator of this install can clear it.
                </Caution>
                <Result action={enableTotp} done="Two-factor is on." working="Checking the code…" />
              </Block>
            ) : (
              <Block
                title="Two-factor is off"
                body="Anyone who can read your email can sign in as you and read every site in every organisation you belong to."
              >
                <div className="mf-set-row">
                  <Button tone="primary" disabled={startTotp.isPending} onClick={() => startTotp.mutate()}>
                    Set up two-factor
                  </Button>
                  <Result action={startTotp} done="" working="Generating a secret…" />
                </div>
              </Block>
            )
          }
        </QueryState>
      </Panel>

      <Panel>
        <PanelHead
          title="Signed-in sessions"
          note="One row per browser holding a live session. The address is stored as a salted hash and shown only as its first characters, because the raw address is never kept."
        />
        <QueryState query={sessions}>
          {(rows) => (
            <>
              <Sheet
                rows={rows}
                columns={sessionColumns}
                getKey={(s) => s.id}
                empty={<Empty title="No other session is open" body="Only this browser is signed in." />}
              />
              <Foot>
                <Button
                  disabled={revokeOthers.isPending || rows.length < 2}
                  onClick={() => revokeOthers.mutate()}
                >
                  Sign out everywhere else
                </Button>
                <span className="mf-set-foot__spacer" />
                <Result action={revokeOthers} done="Other sessions ended." working="Signing out…" />
                {revoke.isError ? (
                  <span className="mf-set-foot__say" data-tone="warn" role="alert">
                    {errText(revoke.error)}
                  </span>
                ) : null}
              </Foot>
            </>
          )}
        </QueryState>
      </Panel>

      <Panel>
        <PanelHead title="Delete your account" />
        <QueryState query={me}>
          {(u) => (
            <Block
              title="This cannot be undone"
              body="Organisations where you are the only owner are deleted with you, and every site inside them goes too."
            >
              <TypedConfirm
                phrase={u.email}
                label="Delete my account"
                busy={deleteAccount.isPending}
                destroys={
                  <>
                    Deletes your account, your profile, your sessions and your API keys. Any organisation you
                    solely own is deleted with it, together with its sites and everything those sites have
                    recorded. Organisations with another owner keep running without you.
                  </>
                }
                onConfirm={() => deleteAccount.mutate()}
              />
              <div className="mf-set-row">
                <Result action={deleteAccount} done="Account deleted." working="Deleting…" />
              </div>
            </Block>
          )}
        </QueryState>
      </Panel>
    </>
  );
}

export default AccountSettings;
