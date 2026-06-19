<script setup>
defineProps({
  type: { type: String, default: 'muted' },
  text: { type: String, default: '' }
})
</script>

<template>
  <span :class="['pill', `pill-${type}`]">{{ text }}</span>
</template>
