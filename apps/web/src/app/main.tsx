/**
 * Where the dashboard starts.
 *
 * ON THE BASENAME. The application is served from two doors (`/app` for the
 * station and `/share` for a public record) and `public/_redirects` rewrites
 * everything under either one back to that door's `index.html`. Both doors run
 * THIS router, and the route table names `/app/…` and `/share/…` in full, so
 * the basename is the site's own root rather than either door. That is not a
 * detail to "tidy up": every screen in this application navigates by absolute
 * path (`ENTRY_PATHS.login` is `/app/login`, the rail's links are `/app/:site/…`),
 * and setting the basename to `/app` would resolve those to `/app/app/login`.
 * What the basename DOES carry is a deployment mounted under a sub-path, which
 * is what `BASE_URL` reports, and which serves both doors correctly at once.
 *
 * ON `createRoot`. Astro mounts this module as a `client:only` island and calls
 * `createRoot` itself, so calling it again at module scope would mount the
 * dashboard twice into the same page. The root component is therefore the
 * default export, and `mount()` is here for any host that is NOT doing the
 * rendering for us: a plain HTML page, a test harness, an embedded build.
 */
import type { ErrorInfo, ReactNode } from "react";
import { Component, StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { BrowserRouter } from "react-router";

import { Providers } from "./providers";
import { AppRoutes } from "./routes";
import { Button, ErrorState } from "../components/react/ui";
import { publicOrigin } from "../lib/api";

/* ── the basename ────────────────────────────────────────────────────────── */

/** `"/"` for a root deployment; `"/analytics"` for one mounted under a path. */
export function resolveBasename(): string {
  const raw = (import.meta.env?.BASE_URL as string | undefined) ?? "/";
  const trimmed = raw.replace(/\/+$/, "");
  return trimmed.length ? trimmed : "/";
}

/* ── the last line of defence ────────────────────────────────────────────── */

interface BoundaryState {
  error: Error | null;
}

/**
 * A blank white page is the one failure this product will not ship. If a screen
 * throws, the dashboard says so plainly, names what it can, and offers the two
 * things that actually recover it: reload, or go back to the site list.
 */
export class AppErrorBoundary extends Component<{ children: ReactNode }, BoundaryState> {
  state: BoundaryState = { error: null };

  static getDerivedStateFromError(error: Error): BoundaryState {
    return { error };
  }

  componentDidCatch(error: Error, info: ErrorInfo) {
    // The operator owns this server; the console is where they will look.
    console.error("[micaforge] a screen failed to render", error, info.componentStack);
  }

  render() {
    const { error } = this.state;
    if (!error) return this.props.children;

    return (
      <div className="mfs-boundary">
        <div className="mfs-boundary__sheet">
          <ErrorState
            title="This screen stopped working"
            detail={
              `The dashboard hit an error it could not recover from on its own${
                error.message ? `: ${error.message}` : "."
              } ` +
              `The full error is in the browser console. This dashboard reads from ${publicOrigin()}.`
            }
          />
          <div className="mfs-rack__foot mfs-rack__foot--centre">
            {/* No "render it again" here: this boundary only catches a failure
                in the dashboard frame itself, and re-rendering the thing that just
                threw is the one recovery that reliably does nothing. */}
            <Button
              tone="primary"
              onClick={() => {
                window.location.reload();
              }}
            >
              Reload the dashboard
            </Button>
            <Button
              onClick={() => {
                window.location.href = "/app";
              }}
            >
              Back to your sites
            </Button>
          </div>
        </div>
      </div>
    );
  }
}

/* ── the root ────────────────────────────────────────────────────────────── */

export function App() {
  return (
    <AppErrorBoundary>
      <BrowserRouter basename={resolveBasename()}>
        <Providers>
          <AppRoutes />
        </Providers>
      </BrowserRouter>
    </AppErrorBoundary>
  );
}

/**
 * Mounts the dashboard into a container. Only for hosts that are not already
 * rendering `App` themselves; the Astro islands at `/app` and `/share` are, so
 * they render the default export instead of calling this.
 */
export function mount(container?: HTMLElement | null): void {
  const el = container ?? document.getElementById("micaforge-root");
  if (!el) {
    console.error("[micaforge] no container to mount into (expected #micaforge-root)");
    return;
  }
  createRoot(el).render(
    <StrictMode>
      <App />
    </StrictMode>,
  );
}

export default App;
