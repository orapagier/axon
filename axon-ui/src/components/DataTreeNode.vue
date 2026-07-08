<template>
  <div class="data-tree-node">
    <div 
      class="dt-row" 
      draggable="true" 
      :style="{ paddingLeft: (depth + 1) * 12 + 'px' }"
      @dragstart.stop="onDragStart($event, field.fullPath)"
    >
      <template v-if="inputMode === 'json'">
        <span class="dt-key">"{{ field.key }}"</span><span class="dt-sep">:</span>
        <span class="dt-val">{{ typeof field.value === 'string' && !field.isObject ? '"' + field.value + '"' : field.value }}</span>
      </template>
      <template v-else>
        <span
          class="dt-type"
          :class="field.type"
          style="margin-right: 6px;"
        >{{ field.type === 'string' ? 'T' : (field.type === 'number' ? '#' : (field.type === 'boolean' ? '✓' : '{}')) }}</span>
        <span
          class="dt-key"
          style="margin-right: 6px;"
        >{{ field.key }}</span>
        <span class="dt-val">{{ field.value }}</span>
      </template>
    </div>
    
    <template v-if="field.children && field.children.length > 0">
      <DataTreeNode 
        v-for="child in field.children" 
        :key="child.fullPath"
        :field="child"
        :depth="depth + 1"
        :input-mode="inputMode"
        :node-label="nodeLabel"
      />
    </template>
  </div>
</template>

<script setup>
const props = defineProps({
  field: { type: Object, required: true },
  depth: { type: Number, default: 0 },
  inputMode: { type: String, default: 'schema' },
  nodeLabel: { type: String, required: true }
})

function onDragStart(event, fullPath) {
  // Bare expression form (no {{ }}), n8n-style: the resolver treats a
  // whole-field bare $node[...] reference exactly like a {{ }}-wrapped one.
  event.dataTransfer.setData('variable', `$node["${props.nodeLabel}"].data.${fullPath}`)
  event.dataTransfer.effectAllowed = 'copy'
}
</script>

<style scoped>
.dt-row {
  display: flex;
  align-items: center;
  height: 24px;
  gap: 8px;
  border-radius: 4px;
  padding-right: 8px;
  cursor: grab;
  transition: background 0.1s;
  font-family: 'JetBrains Mono', 'Fira Code', monospace;
}

.dt-row:hover { background: rgba(99, 102, 241, 0.15); }
.dt-row:active { cursor: grabbing; }

.dt-type {
  font-size: 9px;
  padding: 1px 4px;
  border-radius: 3px;
  background: rgba(255, 255, 255, 0.06);
  color: rgba(255, 255, 255, 0.5);
  min-width: 14px;
  text-align: center;
}

.dt-key { font-size: 11px; color: rgba(255, 255, 255, 0.75); font-weight: 500; }
.dt-val { font-size: 11px; color: rgba(255, 255, 255, 0.4); overflow: hidden; text-overflow: ellipsis; white-space: nowrap; flex: 1; min-width: 0; }

.dt-type.number { color: #6366f1; }
.dt-type.boolean { color: #50fa7b; }
.dt-type.object { color: #feca57; }

/* Global overrides for json-mode inside the tree */
:global(.mode-json) .dt-key { color: #f28c28; }
:global(.mode-json) .dt-sep { color: rgba(255, 255, 255, 0.4); font-size: 11px; margin-left: -5px; }
:global(.mode-json) .dt-val { color: #2c9b8d; }
</style>
