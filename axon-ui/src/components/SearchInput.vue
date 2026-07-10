<script setup>
defineProps({
  placeholder: { type: String, default: 'Search…' },
})

const model = defineModel({ type: String, default: '' })
</script>

<template>
  <div class="filter-bar">
    <input
      v-model="model"
      type="search"
      class="premium-input search-input"
      :placeholder="placeholder"
    >
  </div>
</template>
