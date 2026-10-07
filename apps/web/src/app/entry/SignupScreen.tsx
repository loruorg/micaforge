/**
 * Create an account.
 *
 * A Micaforge instance can be open or invite-only, and most self-hosted ones
 * are invite-only. When registration is closed this screen does not show a form
 * that the server would reject: it explains the door, and points at the way in.
 */
import type { FormEvent } from "react";
import { useState } from "react";
import { Link, useNavigate, useSearchParams } from "react-router";
import { useMutation, useQueryClient } from "@tanstack/react-query";
import { Button } from "../../components/react/ui";
import { api } from "../../lib/api";
import {
  AuthFrame,
  ENTRY_PATHS,
  Field,
  FormError,
  PasswordField,
  Waiting,
  describedBy,
  fieldError,
  formError,
  useInstanceConfig,
} from "./AuthFrame";

interface AuthedUser {
  id: string;
  email: string;
  name: string;
}

const MIN_PASSWORD = 12;

export function SignupScreen() {
  const navigate = useNavigate();
  const queryClient = useQueryClient();
  const [params] = useSearchParams();
  const config = useInstanceConfig();

  const [name, setName] = useState("");
  const [email, setEmail] = useState(params.get("email") ?? "");
  const [password, setPassword] = useState("");
  const [localError, setLocalError] = useState<string | undefined>(undefined);

  const register = useMutation({
    mutationFn: (body: { name: string; email: string; password: string }) =>
      api.post<AuthedUser>("/api/auth/register", body).then((r) => r.data),
    onSuccess: async () => {
      await queryClient.invalidateQueries();
      navigate(ENTRY_PATHS.chooser, { replace: true });
    },
  });

  const busy = register.isPending;
  const err = register.error;

  const submit = (e: FormEvent) => {
    e.preventDefault();
    if (busy) return;
    if (password.length < MIN_PASSWORD) {
      setLocalError(`Use at least ${MIN_PASSWORD} characters.`);
      return;
    }
    setLocalError(undefined);
    register.mutate({ name: name.trim(), email: email.trim(), password });
  };

  if (config.isPending) {
    return (
      <AuthFrame title="Create an account" note="Reading what this instance allows.">
        <Waiting label="Asking the server whether registration is open" lines={2} />
      </AuthFrame>
    );
  }

  if (config.isError) {
    return (
      <AuthFrame
        title="Create an account"
        note="Micaforge could not reach the server to find out whether registration is open."
      >
        <p className="mf-form__body">
          The server at the configured address did not answer. If you are self-hosting, check that
          the Micaforge container is running, then reload this page.
        </p>
        <div className="mf-form__foot">
          <Button onClick={() => config.refetch()}>Ask again</Button>
          <Link className="mf-entry__link mf-entry__link--sm" to={ENTRY_PATHS.login}>
            Sign in instead
          </Link>
        </div>
      </AuthFrame>
    );
  }

  if (!config.data?.registration_open) {
    return (
      <AuthFrame
        title="This instance is invite-only"
        note="Registration is closed here, so there is no form to fill in."
      >
        <p className="mf-form__body">
          An owner or admin of this Micaforge instance has to invite you. When they do, you will get
          a link that opens straight onto a page where you set your name and password. If you
          already have an invitation, open the link from that email rather than this page.
        </p>
        <div className="mf-form__foot">
          <Link className="mf-btn mf-btn--key mf-btn--md" to={ENTRY_PATHS.login}>
            <span className="mf-btn__label">Sign in</span>
          </Link>
          <a className="mf-entry__link mf-entry__link--sm" href="/self-host">
            Run your own instance instead
          </a>
        </div>
      </AuthFrame>
    );
  }

  return (
    <AuthFrame
      title="Create an account"
      note="One account reads every site an organisation gives it access to."
    >
      <form className="mf-form" onSubmit={submit} noValidate>
        {formError(err) ? <FormError>{formError(err)}</FormError> : null}

        <div className="mf-form__fields">
          <Field label="Name" htmlFor="mf-signup-name" error={fieldError(err, "name")}>
            <input
              id="mf-signup-name"
              className="mf-input"
              type="text"
              autoComplete="name"
              value={name}
              disabled={busy}
              required
              aria-invalid={fieldError(err, "name") ? true : undefined}
              aria-describedby={describedBy("mf-signup-name", { error: fieldError(err, "name") })}
              onChange={(e) => setName(e.target.value)}
            />
          </Field>

          <Field label="Email" htmlFor="mf-signup-email" error={fieldError(err, "email")}>
            <input
              id="mf-signup-email"
              className="mf-input"
              type="email"
              inputMode="email"
              autoComplete="username"
              autoCapitalize="off"
              spellCheck={false}
              value={email}
              disabled={busy}
              required
              aria-invalid={fieldError(err, "email") ? true : undefined}
              aria-describedby={describedBy("mf-signup-email", { error: fieldError(err, "email") })}
              onChange={(e) => setEmail(e.target.value)}
            />
          </Field>

          <PasswordField
            id="mf-signup-password"
            label="Password"
            value={password}
            onChange={(v) => {
              setPassword(v);
              if (localError) setLocalError(undefined);
            }}
            autoComplete="new-password"
            disabled={busy}
            hint={`At least ${MIN_PASSWORD} characters. Length does more for you here than punctuation does.`}
            error={localError ?? fieldError(err, "password")}
          />
        </div>

        <div className="mf-form__foot">
          <Button tone="primary" type="submit" disabled={busy}>
            {busy ? "Creating…" : "Create account"}
          </Button>
          <Link className="mf-entry__link mf-entry__link--sm" to={ENTRY_PATHS.login}>
            I already have an account
          </Link>
        </div>
      </form>
    </AuthFrame>
  );
}

export default SignupScreen;
