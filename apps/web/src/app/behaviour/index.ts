/**
 * The behaviour area: events, goals, funnels, journeys, retention.
 *
 * The area's stylesheet is imported here, from the barrel, so a route only has
 * to import the screen it is mounting and the styles arrive with it.
 */
import "../../styles/app/behaviour.css";

export { EventsScreen } from "./EventsScreen";
export { GoalsScreen } from "./GoalsScreen";
export { FunnelsScreen } from "./FunnelsScreen";
export { JourneysScreen } from "./JourneysScreen";
export { RetentionScreen } from "./RetentionScreen";

export { GoalEditor } from "./GoalEditor";
export { FunnelEditor } from "./FunnelEditor";
export { Sankey } from "./Sankey";
export { CohortGrid } from "./CohortGrid";

export type { Goal, GoalKind, GoalConditions, MatchOp, PropCondition } from "./GoalEditor";
export type { Funnel, FunnelStep, StepKind } from "./FunnelEditor";
export type { SankeyNode, SankeyLink, SankeyProps } from "./Sankey";
export type { Cohort, CohortMode, CohortUnit, CohortGridProps } from "./CohortGrid";
