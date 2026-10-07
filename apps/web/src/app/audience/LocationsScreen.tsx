/**
 * Locations: country, then region, then city, drilled in place.
 *
 * The plate and the sheet are one instrument read two ways: the plate carries
 * the shape of the record and the sheet carries its figures, and pointing at
 * either marks the other. The drill is local to this screen, because narrowing
 * from a country to a city is a question about *this* report; clipping the
 * whole record to a place is a separate, deliberate action with its own key.
 */
import { useMemo, useState } from "react";
import { Button, Empty, ErrorState, Panel, PanelHead, Sheet } from "../../components/react/ui";
import { IconChevron } from "../../components/react/icons";
import type { Column } from "../../components/react/ui";
import type { Envelope, Filter } from "../../lib/api";
import { compact, pct } from "../../lib/format";
import { againstLeader } from "../display";
import { useAppState } from "../state";
import {
  AudienceBar,
  Caveat,
  ExportKey,
  FilterTrail,
  count,
  errorDetail,
  n,
  useAudienceQuery,
  useEnvelope,
} from "./PagesScreen";
import type { AudienceRow } from "./PagesScreen";
import { WorldMap, worldName } from "./WorldMap";
import type { MapDatum } from "./WorldMap";
import "../../styles/app/audience.css";

/** The record writes an unset country as blanks; it is a real, honest bucket. */
const isPlaced = (code: string) => !!code && code.trim().length === 2 && code.trim() !== "--";

export function LocationsScreen() {
  const { addFilter, audience } = useAppState();
  const [country, setCountry] = useState<string | null>(null);
  const [region, setRegion] = useState<string | null>(null);
  const [hovered, setHovered] = useState<string | null>(null);
  const machines = audience === "agent";
  const unit = machines ? "fetches" : "visitors";

  const { env, ready } = useEnvelope();

  /* The plate always reads countries, whatever the sheet has drilled into. */
  const countryEnv = useMemo<Envelope>(
    () => ({ ...env, dimension: "country", limit: 300 }),
    [env],
  );
  const countriesQ = useAudienceQuery<AudienceRow>(
    "locations:country",
    "/api/stats/locations",
    countryEnv,
    ready,
  );
  const countryRows = countriesQ.data?.rows ?? [];

  const mapData = useMemo<MapDatum[]>(
    () =>
      countryRows
        .filter((r) => isPlaced(r.key))
        .map((r) => ({
          code: r.key.trim().toUpperCase(),
          label: r.label ?? worldName(r.key.trim().toUpperCase()),
          value: n(r, "visitors") ?? n(r, "pageviews") ?? 0,
          share: n(r, "share"),
        })),
    [countryRows],
  );

  /* The drilled level. Region and city are asked for only once a place is
     chosen, so the screen never fires a query it cannot use. */
  const level: "country" | "region" | "city" = region ? "city" : country ? "region" : "country";

  const drillEnv = useMemo<Envelope>(() => {
    const base = (env.filters ?? []) as Filter[];
    const extra: Filter[] = [];
    if (country) extra.push({ dimension: "country", op: "is", value: country });
    if (region) extra.push({ dimension: "region", op: "is", value: region });
    return {
      ...env,
      dimension: level === "city" ? "city" : "region",
      filters: [...base, ...extra],
    };
  }, [env, country, region, level]);

  const drillQ = useAudienceQuery<AudienceRow>(
    `locations:${level}:${country ?? ""}:${region ?? ""}`,
    "/api/stats/locations",
    drillEnv,
    ready && level !== "country",
  );

  const listQ = level === "country" ? countriesQ : drillQ;
  /* At the country level the plate and the sheet read the same query, so a
     failure there is one failure. It is reported once for the whole bench
     rather than twice, side by side, in the same words. */
  const benchFailed = level === "country" && countriesQ.isError;
  const rows = level === "country" ? countryRows : (drillQ.data?.rows ?? []);

  const placeName = (r: AudienceRow): string => {
    if (level !== "country") return r.label || r.key || "Unknown";
    const code = r.key.trim().toUpperCase();
    return r.label || (isPlaced(code) ? worldName(code) : "Unknown");
  };

  const columns: Column<AudienceRow>[] = [
    {
      key: "place",
      head: level === "country" ? "Country" : level === "region" ? "Region" : "City",
      width: "46%",
      /* Read against the leading place, not against the whole world: a table of
         single-digit shares draws the same stub on every row otherwise. */
      share: againstLeader(rows, (r) => n(r, "share")),
      render: (r) => {
        const code = r.key.trim().toUpperCase();
        return (
          <span
            className="mfa-name"
            data-on={level === "country" && code === hovered ? "true" : undefined}
            title={placeName(r)}
            onPointerEnter={level === "country" ? () => setHovered(code) : undefined}
            onPointerLeave={level === "country" ? () => setHovered(null) : undefined}
          >
            {placeName(r)}
          </span>
        );
      },
    },
    {
      key: "visitors",
      head: machines ? "Clients" : "Visitors",
      numeric: true,
      render: (r) => count(n(r, "visitors") ?? n(r, "pageviews")),
    },
    {
      key: "share",
      head: "Share",
      numeric: true,
      render: (r) => (n(r, "share") == null ? "—" : pct(n(r, "share")!, 1)),
    },
  ];

  const openRow = (r: AudienceRow) => {
    const code = r.key.trim().toUpperCase();
    if (level === "country") {
      if (!isPlaced(code)) return;
      setCountry(code);
      setRegion(null);
      return;
    }
    if (level === "region") {
      setRegion(r.key);
      return;
    }
    // The leaf: nothing further to open, so the click clips the record instead.
    addFilter({ dimension: "city", op: "is", value: r.key } as Filter);
  };

  const clipHere = () => {
    if (region) addFilter({ dimension: "region", op: "is", value: region } as Filter);
    else if (country) addFilter({ dimension: "country", op: "is", value: country } as Filter);
  };

  const emptyForLevel =
    level === "country" ? (
      <Empty
        title="No arrivals were located in this interval"
        body="A country appears as soon as one hit can be placed. Locations are derived from the address at ingest and are never stored on the row."
      />
    ) : level === "region" ? (
      <Empty
        title={`No regions were recorded for ${worldName(country ?? "")}`}
        body="The bundled location database resolves regions for most countries but not all. The country total above is still complete."
      />
    ) : (
      <Empty
        title={`No cities were recorded in ${region}`}
        body="City resolution needs a more precise address match than this traffic gave. The region total is still complete."
      />
    );

  return (
    /* Which pen these sheets are written in: the ink behind a row is the trace
       of the audience being read, never the machine pen on a human table. */
    <div className="mfa-screen" data-trace={machines ? "agent" : "human"}>
      <Panel ruled feed>
        <PanelHead
          title="Locations"
          note="Where the record was written from, resolved at ingest from the address and never stored on the row itself."
          actions={<AudienceBar extra={<ExportKey report="locations:country" env={env} />} />}
        />

        <FilterTrail />

        {machines ? (
          <Caveat>
            A machine fetch is placed by address to a country and no further, because the machine
            trace carries no region and no city. Drilling below country reads the human
            record only.
          </Caveat>
        ) : null}

        {benchFailed ? (
          <ErrorState
            title="Locations did not come back"
            detail={errorDetail(countriesQ.error)}
            onRetry={() => void countriesQ.refetch()}
          />
        ) : (
          <div className="mfa-geo">
            <div className="mfa-geo__plate">
              {countriesQ.isError ? (
                <ErrorState
                  title="The plate did not come back"
                  detail={errorDetail(countriesQ.error)}
                  onRetry={() => void countriesQ.refetch()}
                />
              ) : (
                <WorldMap
                  data={mapData}
                  unit={unit}
                  loading={countriesQ.isLoading}
                  hovered={hovered}
                  selected={country}
                  onHover={setHovered}
                  onSelect={(code) => {
                    setCountry(code);
                    setRegion(null);
                  }}
                />
              )}
            </div>

            <div className="mfa-geo__list">
              <div className="mfa-crumbs">
                {country ? (
                  <>
                    <button
                      type="button"
                      className="mfa-crumbs__step"
                      onClick={() => {
                        setCountry(null);
                        setRegion(null);
                      }}
                    >
                      All countries
                    </button>
                    <IconChevron size={12} className="mfa-crumbs__sep" />
                    {region ? (
                      <>
                        <button
                          type="button"
                          className="mfa-crumbs__step"
                          onClick={() => setRegion(null)}
                        >
                          {worldName(country)}
                        </button>
                        <IconChevron size={12} className="mfa-crumbs__sep" />
                        <span className="mfa-crumbs__here">{region}</span>
                      </>
                    ) : (
                      <span className="mfa-crumbs__here">{worldName(country)}</span>
                    )}
                    <Button tone="quiet" size="sm" onClick={clipHere}>
                      Clip the record here
                    </Button>
                  </>
                ) : (
                  <span className="mfa-crumbs__here">All countries</span>
                )}
              </div>

              {listQ.isError ? (
                <ErrorState
                  title={
                    level === "country"
                      ? "Countries did not come back"
                      : level === "region"
                        ? "Regions did not come back"
                        : "Cities did not come back"
                  }
                  detail={errorDetail(listQ.error)}
                  onRetry={() => void listQ.refetch()}
                />
              ) : (
                <Sheet<AudienceRow>
                  rows={rows}
                  columns={columns}
                  loading={listQ.isLoading}
                  getKey={(r, i) => `${r.key}-${i}`}
                  onRowClick={openRow}
                  empty={emptyForLevel}
                />
              )}
            </div>
          </div>
        )}

        {rows.length && !benchFailed ? (
          <div className="mfa-foot">
            <span>
              {level === "country"
                ? "Click a country, on the plate or in the sheet, to read its regions."
                : level === "region"
                  ? "Click a region to read its cities."
                  : "Click a city to clip the whole record to it."}
            </span>
            <span className="mf-data">{compact(listQ.data?.total ?? rows.length)} rows</span>
          </div>
        ) : null}
      </Panel>
    </div>
  );
}

export default LocationsScreen;
