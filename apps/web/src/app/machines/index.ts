/**
 * AI traffic: the four screens that report on AI crawlers.
 *
 * The area's stylesheet is imported here, so the router only ever has to know
 * about this barrel.
 */
import "../../styles/app/machines.css";

export { AgentsScreen } from "./AgentsScreen";
export { CitationGapScreen } from "./CitationGapScreen";
export { PolicyScreen } from "./PolicyScreen";
export { DecayScreen } from "./DecayScreen";
export { AgentDetail } from "./AgentDetail";
export type { AgentIdentity } from "./AgentDetail";
