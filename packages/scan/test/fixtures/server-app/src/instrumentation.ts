import { createInstrumentation } from "@micaforge/sdk-server/next";

export const { register, client: mf } = createInstrumentation({
  host: "https://analytics.example.com",
  siteId: 1,
  ingestKey: "",
});

mf.track({ name: "next_server_boot" });
