/**
 * useCanvasNode composable - provides node-specific state and methods
 * Based on n8n's useCanvasNode composable
 */

import { inject, computed, ref } from 'vue'
import { CanvasNodeKey } from '../lib/canvas/constants.js'

/**
 * Inject canvas node context from parent
 */
export function useCanvasNode() {
  const context = inject(CanvasNodeKey, null)

  if (!context) {
    // Return default empty values if no context provided
    return {
      id: ref(''),
      data: ref({}),
      label: ref(''),
      selected: ref(false),
      readOnly: ref(false),
      // Computed defaults
      isDisabled: computed(() => false),
      isReadOnly: computed(() => false),
      isSelected: computed(() => false),
      executionStatus: computed(() => 'unknown'),
      hasPinnedData: computed(() => false),
      hasRunData: computed(() => false),
      hasExecutionErrors: computed(() => false),
      executionRunning: computed(() => false),
      executionWaiting: computed(() => false),
      executionWaitingForNext: computed(() => false),
      subtitle: computed(() => ''),
      inputs: computed(() => []),
      outputs: computed(() => []),
      connections: computed(() => ({ input: {}, output: {} })),
      render: computed(() => ({ type: 'default', options: {} })),
    }
  }

  // Extract refs
  const id = context.id
  const data = context.data
  const label = context.label
  const selected = context.selected
  const readOnly = context.readOnly

  // Computed properties based on node data
  const isDisabled = computed(() => data.value?.disabled ?? false)
  const isReadOnly = computed(() => readOnly.value ?? false)
  const isSelected = computed(() => selected.value ?? false)

  const executionStatus = computed(() => data.value?.execution?.status ?? 'unknown')
  const executionRunning = computed(() => data.value?.execution?.running ?? false)
  const executionWaiting = computed(() => data.value?.execution?.waiting ?? false)
  const executionWaitingForNext = computed(() => data.value?.execution?.waitingForNext ?? false)

  const hasPinnedData = computed(() => data.value?.pinnedData?.length > 0 ?? false)
  const hasRunData = computed(() => {
    return (data.value?.runData?.length > 0) || (data.value?.execution?.status === 'success')
  })
  const hasExecutionErrors = computed(() => {
    return data.value?.execution?.status === 'error' || (data.value?.execution?.status === 'failed')
  })

  const subtitle = computed(() => {
    const nodeType = data.value?.type
    if (!nodeType) return ''
    // Simplified subtitle generation
    return nodeType.replace(/([A-Z])/g, ' $1').trim()
  })

  const inputs = computed(() => data.value?.inputs ?? [])
  const outputs = computed(() => data.value?.outputs ?? [])
  const connections = computed(() => data.value?.connections ?? { input: {}, output: {} })
  const render = computed(() => data.value?.render ?? { type: 'default', options: {} })

  return {
    id,
    data,
    label,
    selected,
    readOnly,
    isDisabled,
    isReadOnly,
    isSelected,
    executionStatus,
    executionRunning,
    executionWaiting,
    executionWaitingForNext,
    hasPinnedData,
    hasRunData,
    hasExecutionErrors,
    subtitle,
    inputs,
    outputs,
    connections,
    render,
  }
}
