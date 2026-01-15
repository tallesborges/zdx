<script setup lang="ts">
import { Health, apiGet } from './lib/api'
import { onMounted, ref } from 'vue'

const health = ref<Health | null>(null)

onMounted(async () => {
  health.value = await apiGet<Health>('/api/health')
})
</script>

<template>
  <h1>You did it!</h1>
  <p>
    Visit <a href="https://vuejs.org/" target="_blank" rel="noopener">vuejs.org</a> to read the
    documentation
  </p>
  <div>API Status: {{ health?.ok }}</div>
</template>

<style scoped></style>
