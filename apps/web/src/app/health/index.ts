/**
 * The health area: performance and errors.
 *
 * The area's stylesheet is imported here so that reaching any of these screens
 * through this barrel is enough to bring its styles with it.
 */
import "../../styles/app/health.css";

export { PerformanceScreen } from "./PerformanceScreen";
export { ErrorsScreen, ErrorTrace } from "./ErrorsScreen";
export { ErrorDetail } from "./ErrorDetail";
export { VitalsDistribution, VITALS, VITAL_BY_KEY, RATING_WORD, rateVital } from "./VitalsDistribution";

export type {
  VitalBin,
  VitalHistogram,
  VitalKey,
  VitalRating,
  VitalSpec,
  VitalStat,
  VitalStats,
} from "./VitalsDistribution";
export type {
  PerformanceBreakdown,
  PerformanceRow,
  PerformanceSummary,
} from "./PerformanceScreen";
export type {
  ErrorFacet,
  ErrorFacetKey,
  ErrorGroup,
  ErrorStatus,
  ErrorsResponse,
} from "./ErrorsScreen";
export type { ErrorDetailData, ErrorSession } from "./ErrorDetail";
