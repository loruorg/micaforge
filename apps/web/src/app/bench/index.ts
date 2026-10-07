/**
 * THE BENCH: custom sheets, the query console, feature flags and experiments.
 *
 * The four routes the router mounts are the four screens exported first below.
 * `FlagEditor` and `ExperimentResults` are panels their own index screen opens
 * underneath itself; they take props rather than reading the URL, so they are
 * exported for composition rather than as routes.
 *
 * The area stylesheet is imported here so a router that pulls one screen out of
 * this barrel gets the styles with it.
 */
import "../../styles/app/bench.css";

export { DashboardsScreen } from "./DashboardsScreen";
export { DashboardEditor } from "./DashboardEditor";
export { QueryScreen } from "./QueryScreen";
export { FlagsScreen } from "./FlagsScreen";
export { ExperimentsScreen } from "./ExperimentsScreen";

export { FlagEditor } from "./FlagEditor";
export { ExperimentResults } from "./ExperimentResults";

/* The bench's shared query vocabulary, in case another area wants the same
   condition builder or the same field controls rather than growing its own. */
export {
  DIMENSIONS,
  METRICS,
  BREAKDOWN_METRICS,
  OPS,
  Field,
  FilterBuilder,
  KeyBank,
  NumberInput,
  Reading,
  Select,
  TextArea,
  TextInput,
  Throw,
  WaitStrip,
  dimensionLabel,
  downloadCsv,
  errText,
  metricDef,
  opIsValueless,
  statsKey,
  toCsv,
} from "./QueryScreen";
export type { MetricDef, MetricKey } from "./QueryScreen";

export {
  GRID_COLS,
  VISUALS,
  cardTitle,
  dashboardsKey,
  emptyLayout,
  fetchDashboards,
  newCard,
  newCardId,
  normaliseLayout,
} from "./DashboardsScreen";
export type { CardVis, DashCard, DashLayout, Dashboard } from "./DashboardsScreen";

export {
  RolloutBand,
  blankFlag,
  effectiveVariants,
  flagProblems,
  flagsKey,
  normaliseFlag,
  totalWeight,
} from "./FlagEditor";
export type { FeatureFlag, FlagVariant, Observed } from "./FlagEditor";

export {
  blankExperiment,
  callIt,
  controlOf,
  diffInterval,
  experimentsKey,
  goalsKey,
  normaliseExperiment,
  sampleNeeded,
  twoProportionP,
  wilson,
} from "./ExperimentResults";
export type {
  Arm,
  Call,
  Experiment,
  ExperimentStatus,
  ExperimentVariant,
  Goal,
  Interval,
} from "./ExperimentResults";
