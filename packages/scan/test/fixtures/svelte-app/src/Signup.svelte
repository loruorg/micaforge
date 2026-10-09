<script lang="ts">
  import { micaforge, trackClick } from "@micaforge/sdk/svelte";
  import { track } from "@micaforge/sdk";

  function done() {
    track("svelte_signup_done", { method: "email" });
  }
</script>

<button use:trackClick={{ event: "svelte_cta", props: { color: "blue" } }}>Go</button>
<button use:trackClick={"svelte_simple"}>Simple</button>
<button on:click={() => $micaforge?.track("svelte_store", { n: 2 })}>Store</button>
<button data-track={"svelte_tag"} data-track-variant="b" on:click={done}>Tag</button>
<p class:active={true}>{micaforge ? "on" : "off"}</p>
