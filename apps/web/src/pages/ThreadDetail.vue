<script setup lang="ts">
import { useThread } from '@/composables/useThread'
import { computed, ref, watch } from 'vue'
import { useRoute } from 'vue-router'

const route = useRoute()
const { thread, error, notFound, loading, getParamId, loadThread } = useThread()

const displayId = computed(() => getParamId(route.params.id) ?? 'unknown')
const copiedTimeout = ref<number | null>(null)
const copyStatus = ref<string | null>(null)

watch(
  () => route.params.id,
  (newId) => {
    copyStatus.value = null
    loadThread(newId)
  },
  { immediate: true }
)

function showCopyStatus(message: string) {
  copyStatus.value = message

  if (copiedTimeout.value !== null) {
    clearTimeout(copiedTimeout.value)
  }

  copiedTimeout.value = setTimeout(() => {
    copyStatus.value = null
    copiedTimeout.value = null
  }, 2000)
}

async function copyToClipboard(content: string) {
  try {
    await navigator.clipboard.writeText(content)
    showCopyStatus("Copied")
  } catch (e) {
    showCopyStatus(e instanceof Error ? e.message : "Copy failed")
  }
}

</script>

<template>
  <p v-if="copyStatus"> {{ copyStatus }}</p>
  <router-link to="/threads">Threads</router-link>
  <p v-if="notFound">Thread {{ displayId }} not found</p>
  <p v-else-if="error">Error: {{ error }}</p>
  <p v-else-if="loading">Loading ...</p>
  <div v-else-if="thread">
    <h1>Thread {{ thread.title }}</h1>
    <ul>
      <li v-for="(message, index) in thread.messages" :key="index">
        <span v-text="message.role" style="font-weight: bold;" /> - <span v-text="message.content" />
        <button @click="copyToClipboard(message.content)">Copy</button>
      </li>
    </ul>
  </div>
  <p v-else>Unknown error</p>
</template>
