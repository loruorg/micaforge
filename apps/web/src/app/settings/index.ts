/**
 * The settings and admin area.
 *
 * The router imports screens from here; it never needs to read the files. The
 * area's stylesheet is pulled in as a side effect so a route that mounts a
 * screen always gets its styles with it.
 */
import "../../styles/app/settings.css";

export { SettingsLayout } from "./SettingsLayout";
export { TrackingSetup } from "./TrackingSetup";
export { SiteSettings } from "./SiteSettings";
export { AlertsSettings } from "./AlertsSettings";
export { ReportsSettings } from "./ReportsSettings";
export { ImportsSettings } from "./ImportsSettings";
export { ShareSettings } from "./ShareSettings";
export { DangerZone } from "./DangerZone";
export { AccountSettings } from "./AccountSettings";
export { OrgSettings } from "./OrgSettings";
export { TeamsSettings } from "./TeamsSettings";
export { ApiKeysSettings } from "./ApiKeysSettings";
export { AdminScreen } from "./AdminScreen";
