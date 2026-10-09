import { track, TrackClick, useMicaforge, useTrackEvent } from "@micaforge/sdk/react";
import { CHECKOUT } from "./names";

const PLAN_CHOSEN = "plan_chosen";
const EVENTS = { SEARCH: "search_run" } as const;

export function App({ user, step }: { user: { email: string }; step: number }) {
  const trackEvent = useTrackEvent();
  const mf = useMicaforge();
  return (
    <main>
      <button onClick={() => track("signup", { plan: "pro", seats: 5 })}>Sign up</button>
      <button onClick={() => trackEvent(PLAN_CHOSEN, { plan: "team" })}>Team</button>
      <button onClick={() => mf?.track("Sign-Up", { plan: "free" })}>Sign up again</button>
      <button onClick={() => track(EVENTS.SEARCH, { query_length: 4 })}>Search</button>
      <button data-track="cta_click" data-track-placement="hero">
        Start
      </button>
      <TrackClick event="docs_open" props={{ section: "api" }}>
        <a href="/docs">Docs</a>
      </TrackClick>
      <button onClick={() => track(`step_${step}`)}>Next</button>
      <button onClick={() => track(CHECKOUT)}>Checkout</button>
      <button onClick={() => track("profile_saved", { email: user.email })}>Save</button>
    </main>
  );
}
