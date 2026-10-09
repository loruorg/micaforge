/**
 * Deep links into the dashboard, on the dev server.
 *
 * In production `public/_redirects` rewrites every `/app/*` and `/share/*`
 * address back to that door's `index.html`, and React Router reads the real
 * address off the URL bar. The dev server has no such rule, so reloading on
 * `/app/7/pages` would land on the marketing 404 instead of the pages channel.
 *
 * This is that one rule, applied in dev only. It changes the request Astro
 * answers, never the address in the browser, so the router still sees the URL
 * the operator actually asked for. Nothing here runs at build time.
 */

/** @returns {import("astro").AstroIntegration} */
export default function spaDevRouting() {
  const DOORS = ["/app", "/share", "/status"];

  return {
    name: "micaforge:spa-dev-routing",
    hooks: {
      "astro:server:setup": ({ server }) => {
        server.middlewares.use((req, _res, next) => {
          const raw = req.url ?? "/";
          const [pathname, query] = raw.split("?");
          const door = DOORS.find((d) => pathname === d || pathname.startsWith(`${d}/`));

          // Only a deep link needs rewriting: the door itself, and any real file
          // beneath it, are served as they are.
          if (door && pathname !== `${door}/` && !/\.[a-z0-9]+$/i.test(pathname)) {
            req.url = `${door}/${query ? `?${query}` : ""}`;
          }
          next();
        });
      },
    },
  };
}
