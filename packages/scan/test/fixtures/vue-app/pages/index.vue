<script setup>
// Nuxt imports composables for you: no import line for useMicaforge.
const mf = useMicaforge();

function go() {
  mf.track("nuxt_cta", { page: "home" });
}

useMicaforge().track("nuxt_page_ready");
</script>

<template>
  <button @click="go">Go</button>
</template>
