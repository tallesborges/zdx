<script setup lang="ts">
import { ApiError, apiGet, type ThreadDetail } from '@/lib/api'
import { onMounted, ref } from 'vue'
import { useRoute } from 'vue-router'

const route = useRoute()
const id = getParamId(route.params.id)
const thread = ref<ThreadDetail | null>(null)
const error = ref<string | null>(null)
const notFound = ref<boolean>(false)

onMounted(async () => {
  try {
    error.value = null
    notFound.value = false

    if (id === null) {
      error.value = "Invalid thread id"
      return
    }

    thread.value = await apiGet<ThreadDetail>(`/api/threads/${id}`)
  } catch (err) {
    if (err instanceof ApiError && err.status === 404) {
      notFound.value = true
    } else {
      error.value = (err as Error).message
    }
  }
})

function getParamId(val: unknown): string | null {
  if (typeof val === 'string') return val

  if (Array.isArray(val) && val.length > 0 && typeof val[0] === 'string') {
    return val[0]
  }

  return null
}

</script>

<template>
  <router-link to="/threads">Threads</router-link>
  <p v-if="notFound">Thread {{ id }} not found</p>
  <p v-else-if="error">Error: {{ error }}</p>
  <div v-else-if="thread">
    <h1>Thread {{ thread.title }}</h1>
    <ul>
      <li v-for="(message, index) in thread.messages" :key="index">
        <span v-text="message.role" style="font-weight: bold;" /> - <span v-text="message.content" />
      </li>
    </ul>
  </div>
  <p v-else>Loading ...</p>
</template>
