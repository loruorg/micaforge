// @ts-check
import { defineConfig } from "astro/config";
import react from "@astrojs/react";
import mdx from "@astrojs/mdx";
import sitemap from "@astrojs/sitemap";
import tailwind from "@tailwindcss/vite";
import spaDevRouting from "./spa-dev.mjs";

const site = process.env.PUBLIC_SITE_URL ?? "https://micaforge.dev";

export default defineConfig({
  site,
  output: "static",
  markdown: {
    // No highlighter. Shiki writes its theme's colours into the markup as inline
    // hexes, and this product does not ship a colour that is not a token — least of
    // all a violet keyword on a soot ground. Samples are set in the data face, in one
    // ink, framed by the code block, which is how a station's log is written anyway.
    syntaxHighlight: false,
  },
  integrations: [react(), mdx(), sitemap(), spaDevRouting()],
  vite: { plugins: [tailwind()] },
  build: { inlineStylesheets: "auto" },
  devToolbar: { enabled: false },
});
