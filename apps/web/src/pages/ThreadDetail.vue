<script setup lang="ts">
import { useThread } from '@/composables/useThread'
import { computed, watch } from 'vue'
import { useRoute } from 'vue-router'

const route = useRoute()
const { thread, error, notFound, loading, getParamId, loadThread } = useThread()

const displayId = computed(() => getParamId(route.params.id) ?? 'unknown')

watch(
  () => route.params.id,
  (newId) => { loadThread(newId) },
  { immediate: true }
)

</script>

<template>
  <router-link to="/threads">Threads</router-link>
  <p v-if="notFound">Thread {{ displayId }} not found</p>
  <p v-else-if="error">Error: {{ error }}</p>
  <p v-else-if="loading">Loading ...</p>
  <div v-else-if="thread">
    <h1>Thread {{ thread.title }}</h1>
    <ul>
      <li v-for="(message, index) in thread.messages" :key="index">
        <span v-text="message.role" style="font-weight: bold;" /> - <span v-text="message.content" />
      </li>
    </ul>
  </div>
  <p v-else>Unknown error</p>
</template>
