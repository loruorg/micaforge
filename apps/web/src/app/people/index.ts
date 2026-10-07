/**
 * PEOPLE: sessions, visitors, replay.
 *
 * The barrel the router mounts. `SessionsScreen.tsx` additionally holds this
 * area's shared floor (the session shapes, the site-timezone formatters, the
 * `Hand` mark, the pager and the envelope helper), and `VisitorDetail.tsx`
 * holds the erasure dialog the replay screens reuse, so both are re-exported
 * here rather than reached for through a deep import.
 */
import "../../styles/app/people.css";

export { SessionsScreen } from "./SessionsScreen";
export { SessionDetail } from "./SessionDetail";
export { VisitorsScreen } from "./VisitorsScreen";
export { VisitorDetail } from "./VisitorDetail";
export { ReplayScreen } from "./ReplayScreen";
export { ReplayPlayer, ReplayStage } from "./ReplayPlayer";

export { Hand, Pager, Subject, useBase, useEnvelope, statsKey } from "./SessionsScreen";
export { ErasureDialog, IconErase } from "./VisitorDetail";

export type { SessionHit, SessionRow } from "./SessionsScreen";
export type { VisitorRow } from "./VisitorsScreen";
export type { ReplayRow } from "./ReplayScreen";
export type { ReplayEvent } from "./ReplayPlayer";
export type { ErasureLine } from "./VisitorDetail";
