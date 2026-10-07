/**
 * The audience area: pages, sources, locations, devices.
 *
 * Four screens that read the record from the reader's side, each one honest
 * about what the machine trace can and cannot answer. The router imports only
 * what this barrel names.
 */
export { PagesScreen } from "./PagesScreen";
export { SourcesScreen } from "./SourcesScreen";
export { LocationsScreen } from "./LocationsScreen";
export { DevicesScreen } from "./DevicesScreen";
export { WorldMap, worldName, worldHasShape, WORLD_DRAWN } from "./WorldMap";
export type { MapDatum } from "./WorldMap";
