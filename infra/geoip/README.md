# infra/geoip

Put a MaxMind-compatible database here and Micaforge resolves geography. Leave it
empty and Micaforge does not. That is the whole mechanism.

The directory is mounted read-only into the server container at `/geoip`, so a
file named `GeoLite2-City.mmdb` here is `/geoip/GeoLite2-City.mmdb` there. Point
the two variables in `.env` at those paths:

    MICAFORGE_GEOIP_CITY_PATH=/geoip/GeoLite2-City.mmdb
    MICAFORGE_GEOIP_ASN_PATH=/geoip/GeoLite2-ASN.mmdb

Then `docker compose up -d micaforge`.

## Why the file is not shipped

Micaforge makes no outbound call at runtime, for anything, including this
(PRODUCT.md). A lookup service would break that promise and would also hand a
third party the addresses of your readers. So the data has to be local, and the
databases that carry it are licensed by their publishers rather than by us. You
download the one you are entitled to; we do not redistribute it.

MaxMind's GeoLite2 files and the DB-IP and IPinfo free databases are all in the
same MMDB format and all work. Check the licence terms of whichever you pick.

## What you lose without it

Nothing stops working.

- **No city database.** Country only, which most hosts supply in a request
  header anyway. Region and city columns stay empty.
- **No ASN database.** No `asn_org`, and no datacenter detection. Agent
  verification falls back to user-agent patterns and reverse DNS, which is
  weaker: it can still name an agent, and it can no longer prove that a fetch
  claiming to be GPTBot came from an address OpenAI publishes.

Files in this directory other than this README are ignored by git.
