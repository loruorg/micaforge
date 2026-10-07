//! IP → place, network, and whether the network is a datacentre.
//!
//! A self-hosted install must run with no outbound call at runtime
//! (`PRODUCT.md`, Constraints), so geolocation is a local MaxMind-format
//! database read once at startup and queried in-process. There is no hosted
//! lookup service and no fallback that reaches the network.
//!
//! Both databases are optional, and a missing one is an ordinary operating
//! state rather than a failure: an operator who has not downloaded a
//! GeoLite2 file still gets a working analytics install, with empty country
//! and region columns and every other number intact. [`Geo::open`] therefore
//! cannot fail. It warns once, at startup, where an operator will see it, and
//! then stays quiet — a warning per event would drown the ingest log at exactly
//! the volume where logs matter least.
//!
//! The ASN database earns its place twice over. It names the network an event
//! came from, and it is how `is_datacenter` is decided, which is the first
//! filter between a person on a home connection and a script on a rented VM.
//! That flag is a signal for the bot classifier, not a verdict: plenty of real
//! people browse through a VPN that terminates in a datacentre, so nothing in
//! Micaforge drops a hit for it.

use std::net::IpAddr;
use std::sync::Once;

use maxminddb::{geoip2, Reader};
use serde::{Deserialize, Serialize};

/// The opened GeoIP databases, shared for the life of the process.
///
/// Held behind an `Arc` by the server and queried concurrently: `lookup` takes
/// `&self` and the reader is read-only after construction, so no lock is
/// needed on the ingest path.
#[derive(Debug)]
pub struct Geo {
    city: Option<Reader<Vec<u8>>>,
    asn: Option<Reader<Vec<u8>>>,
}

/// Everything an IP address resolved to.
///
/// Note what is *not* here: the address itself. The IP is used to derive these
/// fields and is then dropped, and no human event row ever carries it
/// (`docs/CONTRACT.md` §7.1).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Location {
    /// ISO-3166-1 alpha-2, uppercase. Empty when unknown.
    pub country: String,
    /// ISO-3166-2 (`US-CA`) when both halves are known, otherwise the
    /// subdivision's English name. This is the key the choropleth map joins
    /// its geometry on, which is why it is a code and not a display string.
    pub region: String,
    /// City name in English. Empty when unknown.
    pub city: String,
    /// City-level latitude. `0.0` when unknown, which is also a real place in
    /// the Gulf of Guinea, so the map treats `country == ""` as the null case
    /// rather than testing the coordinates.
    pub lat: f32,
    /// City-level longitude, with the same caveat as [`Location::lat`].
    pub lon: f32,
    /// Autonomous system number, `0` when unknown.
    pub asn: u32,
    /// Autonomous system organisation, as MaxMind spells it.
    pub asn_org: String,
    /// The ASN belongs to a hosting or cloud provider. See
    /// [`is_datacenter_org`].
    pub is_datacenter: bool,
}

/// Organisation-name fragments that mean "rented compute", lowercase.
///
/// Matched as substrings of the ASN organisation, which is why the entries are
/// distinctive rather than short: `"ovh"` alone would match `"Novhost"`, and a
/// false positive here quietly reclassifies a real reader as a machine. The
/// list is deliberately conservative and deliberately incomplete — it is a
/// hint, and the cost of missing a provider is one unflagged row, while the
/// cost of a bad match is an under-counted audience.
const DATACENTER_ORGS: &[&str] = &[
    "amazon",
    "aws",
    "google cloud",
    "google llc",
    "microsoft",
    "azure",
    "digitalocean",
    "hetzner",
    "ovh sas",
    "ovh hosting",
    "linode",
    "akamai connected cloud",
    "vultr",
    "choopa",
    "constant company",
    "cloudflare",
    "fastly",
    "contabo",
    "scaleway",
    "online s.a.s",
    "oracle",
    "alibaba",
    "aliyun",
    "tencent",
    "m247",
    "datacamp",
    "leaseweb",
    "hostinger",
    "godaddy",
    "namecheap",
    "rackspace",
    "equinix",
    "digital ocean",
    "upcloud",
    "netcup",
    "ionos",
    "strato",
    "gcore",
    "stackpath",
    "bunnyway",
    "fly.io",
    "heroku",
    "packet host",
    "servers.com",
    "worldstream",
    "psychz",
    "colocrossing",
    "quadranet",
    "zenlayer",
    "hivelocity",
    "webnx",
    "hostwinds",
    "interserver",
    "buyvm",
    "frantech",
    "kamatera",
    "time4vps",
    "cherry servers",
    "latitude.sh",
    "vpsserver",
    "clouvider",
];

/// Warn at most once about the city database.
static CITY_WARNED: Once = Once::new();
/// Warn at most once about the ASN database.
static ASN_WARNED: Once = Once::new();

impl Geo {
    /// Open the databases, or degrade quietly.
    ///
    /// `city_path` and `asn_path` are `MICAFORGE_GEOIP_CITY_PATH` and
    /// `MICAFORGE_GEOIP_ASN_PATH`. `None`, a missing file, or a file that is
    /// not a valid MaxMind database all produce the same result: a `Geo` that
    /// answers every lookup with an empty [`Location`], plus one warning.
    ///
    /// Whole databases are read into memory rather than memory-mapped. A
    /// GeoLite2 City file is on the order of 70 MB, the container it runs in
    /// has that to spare, and an owned buffer means a lookup can never fault on
    /// a file an operator swapped underneath the process during an update.
    pub fn open(city_path: Option<&str>, asn_path: Option<&str>) -> Geo {
        Geo {
            city: open_one(city_path, "city", &CITY_WARNED),
            asn: open_one(asn_path, "asn", &ASN_WARNED),
        }
    }

    /// A `Geo` with no databases at all.
    ///
    /// Used by tests and by the importer, where rows already carry the
    /// geography they were exported with and re-deriving it from an IP that no
    /// longer exists would be wrong.
    pub fn empty() -> Geo {
        Geo {
            city: None,
            asn: None,
        }
    }

    /// True when at least one database is loaded.
    ///
    /// The health endpoint reports this so that "every country is blank" is
    /// diagnosable without reading the startup log.
    pub fn is_loaded(&self) -> bool {
        self.city.is_some() || self.asn.is_some()
    }

    /// Resolve one address.
    ///
    /// Private, loopback and link-local addresses return an empty location
    /// without touching the databases. They are not a geographic answer, and on
    /// a self-hosted install behind a reverse proxy they are the addresses a
    /// misconfigured `X-Forwarded-For` produces, so answering them with a
    /// confident country would be worse than answering with nothing.
    pub fn lookup(&self, ip: IpAddr) -> Location {
        let mut location = Location::default();
        if !is_public(ip) {
            return location;
        }

        if let Some(reader) = &self.city {
            if let Ok(Some(record)) = reader.lookup::<geoip2::City>(ip) {
                if let Some(country) = record.country.as_ref().and_then(|c| c.iso_code) {
                    location.country = country.to_ascii_uppercase();
                }
                if let Some(subdivision) = record.subdivisions.as_ref().and_then(|s| s.first()) {
                    let region = match subdivision.iso_code {
                        Some(code) if !location.country.is_empty() => {
                            format!("{}-{}", location.country, code)
                        }
                        Some(code) => code.to_string(),
                        None => english_name(subdivision.names.as_ref()),
                    };
                    location.region = region;
                }
                if let Some(city) = record.city.as_ref() {
                    location.city = english_name(city.names.as_ref());
                }
                if let Some(position) = record.location.as_ref() {
                    location.lat = position.latitude.unwrap_or(0.0) as f32;
                    location.lon = position.longitude.unwrap_or(0.0) as f32;
                }
            }
        }

        if let Some(reader) = &self.asn {
            if let Ok(Some(record)) = reader.lookup::<geoip2::Asn>(ip) {
                location.asn = record.autonomous_system_number.unwrap_or(0);
                if let Some(org) = record.autonomous_system_organization {
                    location.asn_org = org.to_string();
                    location.is_datacenter = is_datacenter_org(org);
                }
            }
        }

        location
    }
}

impl Default for Geo {
    fn default() -> Self {
        Geo::empty()
    }
}

/// Does this organisation name belong to a hosting or cloud provider?
///
/// Exposed because the same question is asked of ASN organisations that arrive
/// from a log shipper, where the enrichment already happened upstream and only
/// the name survives.
pub fn is_datacenter_org(org: &str) -> bool {
    let lower = org.to_ascii_lowercase();
    DATACENTER_ORGS.iter().any(|needle| lower.contains(needle))
}

/// Open one database, warning exactly once if it cannot be used.
fn open_one(path: Option<&str>, label: &str, warned: &Once) -> Option<Reader<Vec<u8>>> {
    let path = path.map(str::trim).filter(|p| !p.is_empty())?;

    match Reader::open_readfile(path) {
        Ok(reader) => {
            tracing::info!(database = label, path, "geoip database loaded");
            Some(reader)
        }
        Err(error) => {
            warned.call_once(|| {
                tracing::warn!(
                    database = label,
                    path,
                    %error,
                    "geoip database could not be opened; location columns will be empty"
                );
            });
            None
        }
    }
}

/// MaxMind ships a name per language; English is the one the dashboard renders.
fn english_name(names: Option<&std::collections::BTreeMap<&str, &str>>) -> String {
    names
        .and_then(|n| n.get("en"))
        .map(|n| (*n).to_string())
        .unwrap_or_default()
}

/// Is this an address that could have come from the public internet?
fn is_public(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            !(v4.is_private()
                || v4.is_loopback()
                || v4.is_link_local()
                || v4.is_broadcast()
                || v4.is_documentation()
                || v4.is_unspecified()
                // 100.64.0.0/10, carrier-grade NAT: shared address space, and
                // what a container network hands out often enough to matter.
                || (v4.octets()[0] == 100 && (64..128).contains(&v4.octets()[1])))
        }
        IpAddr::V6(v6) => {
            !(v6.is_loopback()
                || v6.is_unspecified()
                // fc00::/7 unique-local and fe80::/10 link-local. `is_unique_local`
                // and `is_unicast_link_local` are still unstable, so the prefixes
                // are checked directly rather than pulling in a nightly feature.
                || (v6.segments()[0] & 0xfe00) == 0xfc00
                || (v6.segments()[0] & 0xffc0) == 0xfe80)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_database_is_not_an_error() {
        let geo = Geo::open(Some("/nonexistent/GeoLite2-City.mmdb"), None);
        assert!(!geo.is_loaded());
        let location = geo.lookup("8.8.8.8".parse().unwrap());
        assert_eq!(location, Location::default());
    }

    #[test]
    fn private_addresses_are_never_located() {
        let geo = Geo::empty();
        for ip in [
            "127.0.0.1",
            "10.1.2.3",
            "192.168.0.7",
            "172.16.4.1",
            "100.64.0.1",
            "::1",
            "fd00::1",
            "fe80::1",
        ] {
            assert!(
                !is_public(ip.parse().unwrap()),
                "{ip} should not be treated as public"
            );
            assert_eq!(geo.lookup(ip.parse().unwrap()), Location::default());
        }
    }

    #[test]
    fn public_addresses_are_looked_up() {
        assert!(is_public("8.8.8.8".parse().unwrap()));
        assert!(is_public("2001:4860:4860::8888".parse().unwrap()));
    }

    #[test]
    fn hosting_providers_are_flagged() {
        assert!(is_datacenter_org("AMAZON-02"));
        assert!(is_datacenter_org("Hetzner Online GmbH"));
        assert!(is_datacenter_org("OVH SAS"));
        assert!(is_datacenter_org("DIGITALOCEAN-ASN"));
        assert!(is_datacenter_org("Cloudflare, Inc."));
    }

    #[test]
    fn consumer_networks_are_not_flagged() {
        assert!(!is_datacenter_org("Comcast Cable Communications, LLC"));
        assert!(!is_datacenter_org("Deutsche Telekom AG"));
        assert!(!is_datacenter_org(
            "MEO - SERVICOS DE COMUNICACOES E MULTIMEDIA"
        ));
        assert!(!is_datacenter_org("Vodafone Portugal"));
    }
}
