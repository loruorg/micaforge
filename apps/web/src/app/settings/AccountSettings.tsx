/**
 * ACCOUNT: who is reading the record.
 *
 * The two-factor and session panels are written the way the rest of this
 * product writes about privacy: state exactly what is stored and what it can
 * do, then give the control. The recovery codes are shown once and the screen
 * says so before it generates them, not after.
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
  const [email, setEmail] = useState<string | null>(null);
  const [avatar, setAvatar] = useState<string | null>(null);
  const profile = {
    name: name ?? me.data?.name ?? "",
    email: email ?? me.data?.email ?? "",
    avatar_url: avatar ?? me.data?.avatar_url ?? "",
  };
  const profileDirty =
    !!me.data &&
    (profile.name !== me.data.name ||
      profile.email !== me.data.email ||
      profile.avatar_url !== (me.data.avatar_url ?? ""));

  const saveProfile = useMutation({
    mutationFn: () => api.patch<Me>("/api/auth/me", profile),
    onSuccess: (r) => {
      qc.setQueryData(["account", "me"], r.data);
      setName(null);
      setEmail(null);
      setAvatar(null);
    },
  });

  const resend = useMutation({
    mutationFn: () => api.post<null>("/api/auth/verify/resend"),
  });

  /* password ------------------------------------------------------------ */
  const [current, setCurrent] = useState("");
  const [next, setNext] = useState("");
  const [again, setAgain] = useState("");
  const tooShort = next.length > 0 && next.length < 12;
  const mismatch = again.length > 0 && next !== again;

  const changePassword = useMutation({
    mutationFn: () => api.post<null>("/api/auth/password", { current, next }),
    onSuccess: () => {
      setCurrent("");
      setNext("");
      setAgain("");
    },
  });

  /* two-factor ---------------------------------------------------------- */
  const [enrolling, setEnrolling] = useState<TotpStart | null>(null);
  const [code, setCode] = useState("");
  const [recovery, setRecovery] = useState<string[] | null>(null);
  const [totpPassword, setTotpPassword] = useState("");

  const startTotp = useMutation({
    mutationFn: () => api.post<TotpStart>("/api/auth/totp/start").then((r) => r.data),
    onSuccess: (d) => setEnrolling(d),
  });
  const enableTotp = useMutation({
    mutationFn: () =>
      api.post<{ recovery_codes: string[] }>("/api/auth/totp/enable", { code }).then((r) => r.data),
    onSuccess: (d) => {
      setEnrolling(null);
      setCode("");
      setRecovery(d.recovery_codes ?? []);
      void qc.invalidateQueries({ queryKey: ["account", "me"] });
    },
  });
  const disableTotp = useMutation({
    mutationFn: () => api.post<null>("/api/auth/totp/disable", { password: totpPassword }),
    onSuccess: () => {
      setTotpPassword("");
      void qc.invalidateQueries({ queryKey: ["account", "me"] });
    },
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
                  hint="Used to sign in and to receive reports. Changing it sends a verification mail to the new address; the old one keeps working until the new one is confirmed."
                  htmlFor="acc-email"
                >
                  <TextInput
                    id="acc-email"
                    type="email"
                    data
                    value={profile.email}
                    onChange={(e) => setEmail(e.target.value)}
                  />
                  <div className="mf-set-row">
                    {u.email_verified_at ? (
                      <Status state="done" label={`Verified ${ago(u.email_verified_at)}`} />
                    ) : (
                      <>
                        <Status state="waiting" label="Not verified" />
                        <Button size="sm" tone="quiet" disabled={resend.isPending} onClick={() => resend.mutate()}>
                          Send the mail again
                        </Button>
                        <Result action={resend} done="Sent." working="Sending…" />
                      </>
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
                    setEmail(null);
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
          title="Password"
          note="At least twelve characters. Length is the only rule; Micaforge does not demand a symbol and a digit and a capital."
        />
        <Spec>
          <SpecRow name="Current password" htmlFor="pw-current">
            <TextInput
              id="pw-current"
              type="password"
              autoComplete="current-password"
              value={current}
              onChange={(e) => setCurrent(e.target.value)}
            />
          </SpecRow>
          <SpecRow name="New password" htmlFor="pw-next">
            <Field error={tooShort ? "Twelve characters or more." : undefined}>
              <TextInput
                id="pw-next"
                type="password"
                autoComplete="new-password"
                aria-invalid={tooShort || undefined}
                value={next}
                onChange={(e) => setNext(e.target.value)}
              />
            </Field>
          </SpecRow>
          <SpecRow name="New password again" htmlFor="pw-again">
            <Field error={mismatch ? "The two do not match." : undefined}>
              <TextInput
                id="pw-again"
                type="password"
                autoComplete="new-password"
                aria-invalid={mismatch || undefined}
                value={again}
                onChange={(e) => setAgain(e.target.value)}
              />
            </Field>
          </SpecRow>
        </Spec>
        <Foot>
          <Button
            tone="primary"
            disabled={
              !current || next.length < 12 || next !== again || changePassword.isPending
            }
            onClick={() => changePassword.mutate()}
          >
            Change the password
          </Button>
          <span className="mf-set-foot__spacer" />
          <Result action={changePassword} done="Password changed. Other sessions stay signed in." />
        </Foot>
      </Panel>

      <Panel>
        <PanelHead
          title="Two-factor"
          note="A time-based code from an authenticator app, in addition to the password. The secret is sealed at rest and never leaves the server after enrolment."
        />
        <QueryState query={me}>
          {(u) =>
            recovery ? (
              <Block
                title="Save these recovery codes now"
                body="Each one signs you in once if you lose the authenticator. This is the only time they are shown."
              >
                <CopyField label="Recovery codes" value={recovery.join("\n")} />
                <div className="mf-set-row">
                  <Button tone="primary" onClick={() => setRecovery(null)}>
                    I have saved them
                  </Button>
                </div>
              </Block>
            ) : u.totp_enabled ? (
              <Block title="Two-factor is on" body="Turning it off needs your password, so a borrowed session cannot do it.">
                <div className="mf-set-row">
                  <Field label="Password" htmlFor="totp-pw">
                    <TextInput
                      id="totp-pw"
                      type="password"
                      autoComplete="current-password"
                      value={totpPassword}
                      onChange={(e) => setTotpPassword(e.target.value)}
                    />
                  </Field>
                  <Button
                    tone="danger"
                    disabled={!totpPassword || disableTotp.isPending}
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
                  Recovery codes are generated when you finish and shown once. Have somewhere to put them.
                </Caution>
                <Result action={enableTotp} done="Two-factor is on." working="Checking the code…" />
              </Block>
            ) : (
              <Block
                title="Two-factor is off"
                body="Anyone with your password can read every site in every organisation you belong to."
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
