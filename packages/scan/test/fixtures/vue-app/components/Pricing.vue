<script setup lang="ts">
import { useMicaforge } from "@micaforge/sdk/vue";

const { track } = useMicaforge();
const items = ["a", "b"];
</script>

<template>
  <section>
    <button @click="track('plan_selected', { plan: 'pro' })">Pro</button>
    <button v-mf="'pricing_faq_open'">FAQ</button>
    <button v-mf="{ event: 'contact_sales', props: { size: 'enterprise' } }">Contact</button>
    <a :data-track="'outbound_docs'" data-track-target="docs" href="/docs">Docs</a>
    <button @click="$micaforge.track('vue_global', { count: 1 })">Global</button>
    <li v-for="item of items" :key="item">{{ item }}</li>
  </section>
</template>
