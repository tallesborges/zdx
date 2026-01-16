<script setup lang="ts">
import { apiGet, type ThreadDetail } from '@/lib/api'
import { onMounted, ref } from 'vue'
import { useRoute } from 'vue-router'

const route = useRoute()
const id = route.params.id as string
const thread = ref<ThreadDetail | null>(null)
const error = ref<string | null>(null)

onMounted(async () => {
  try {
    thread.value = await apiGet<ThreadDetail>(`/api/threads/${id}`)
  } catch (err) {
    error.value = (err as Error).message
  }
})
</script>

<template>
  <p v-if="error">Error: {{ error }}</p>
  <div v-else-if="thread">
    <h1>Thread {{ thread.title }}</h1>
    <ul>
      <li v-for="(message, index) in thread.messages" :key="index">
        {{ message.role }}: {{ message.content }}
      </li>
    </ul>
  </div>
  <p v-else>Loading ...</p>
</template>
