<script setup lang="ts">
import { computed, onMounted, ref } from 'vue'
import { useThreads } from '@/composables/useThreads'

const { threads, error, loading, loadThreads } = useThreads()
const query = ref('')
const filtered = computed(() => {
  const list = threads.value ?? []
  const q = query.value.trim().toLowerCase()

  if (!q) return list

  return list.filter((t) => t.title.toLowerCase().includes(q))
})

const displayThreads = computed(() => {
  return filtered.value.map((t) => ({
    ...t,
    displayUpdatedAt: t.updatedAt ? new Date(t.updatedAt).toLocaleString() : "—"
  }))
})

onMounted(() => {
  loadThreads()
})
</script>

<template>
  <router-link to="/">Home</router-link>
  <button @click="loadThreads()" :disabled="loading"> {{ loading ? "Reloading ..." : "Reload" }}</button>

  <h1>Thread List </h1>

  <input v-model="query" placeholder="Search" />

  <div v-if="error">
    <p>Error loading the threads: {{ error }} </p>
  </div>
  <p v-else-if="loading"> Loading ...</p>
  <p v-else-if="displayThreads.length === 0"> Empty threads </p>
  <ul v-else>
    <li v-for="thread in displayThreads" :key="thread.id" class="threads-container">
      <router-link :to="`/threads/${thread.id}`" class="thread-row">
        <span> {{ thread.title }} </span>
        <span> {{ thread.displayUpdatedAt }}</span>
      </router-link>
    </li>
  </ul>
</template>

<style scoped>
.threads-container {
  max-width: 800px;
}

.thread-row {
  display: flex;
  justify-content: space-between;
  align-items: center;
  gap: 8px;
  width: 100%;
}
</style>
