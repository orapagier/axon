/**
 * useNodeConnections composable - manages node connections
 * Based on n8n's useNodeConnections composable
 */

import { computed } from 'vue'
import { NodeConnectionTypes, CanvasConnectionMode } from '../lib/canvas/constants.js'

export function useNodeConnections({ inputs, outputs, connections }) {
  // Main inputs (type: 'main')
  const mainInputs = computed(() => {
    const inputList = inputs.value || []
    return inputList
      .map((input, index) => ({ ...input, index }))
      .filter((input) => input.type === NodeConnectionTypes.Main)
  })

  // Main outputs (type: 'main')
  const mainOutputs = computed(() => {
    const outputList = outputs.value || []
    return outputList
      .map((output, index) => ({ ...output, index }))
      .filter((output) => output.type === NodeConnectionTypes.Main)
  })

  // Non-main inputs
  const nonMainInputs = computed(() => {
    const inputList = inputs.value || []
    return inputList
      .map((input, index) => ({ ...input, index }))
      .filter((input) => input.type !== NodeConnectionTypes.Main)
  })

  // Non-main outputs
  const nonMainOutputs = computed(() => {
    const outputList = outputs.value || []
    return outputList
      .map((output, index) => ({ ...output, index }))
      .filter((output) => output.type !== NodeConnectionTypes.Main)
  })

  // Required non-main inputs
  const requiredNonMainInputs = computed(() => {
    return nonMainInputs.value.filter((input) => input.required)
  })

  // Connection counts for main inputs
  const mainInputConnections = computed(() => {
    const result = []
    mainInputs.value.forEach((input) => {
      const conns = connections.value?.input?.[input.type]?.[input.index] || []
      result.push(...conns)
    })
    return result
  })

  // Connection counts for main outputs
  const mainOutputConnections = computed(() => {
    const result = []
    mainOutputs.value.forEach((output) => {
      const conns = connections.value?.output?.[output.type]?.[output.index] || []
      result.push(...conns)
    })
    return result
  })

  // Check if a connection is valid
  const isValidConnection = (connection) => {
    const { source, target, sourceHandle, targetHandle } = connection

    // Can't connect to self
    if (source === target) return false

    // Must have handles defined
    if (!sourceHandle || !targetHandle) return false

    return true
  }

  return {
    mainInputs,
    mainOutputs,
    nonMainInputs,
    nonMainOutputs,
    requiredNonMainInputs,
    mainInputConnections,
    mainOutputConnections,
    isValidConnection,
  }
}
