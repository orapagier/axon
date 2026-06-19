/**
 * Canvas utility functions based on n8n's architecture
 */

import { CanvasConnectionMode } from './constants.js'

/**
 * Create a unique handle ID for a connection endpoint
 */
export function createCanvasConnectionHandleString({ mode, type, index }) {
  const prefix = mode === CanvasConnectionMode.Input ? 'input' : 'output'
  return `${prefix}_${type}_${index}`
}

/**
 * Parse a handle ID back into its components
 */
export function parseCanvasConnectionHandleString(handleId) {
  if (!handleId) return null

  const parts = handleId.split('_')
  if (parts.length !== 3) return null

  const [prefix, type, indexStr] = parts
  return {
    mode: prefix === 'input' ? CanvasConnectionMode.Input : CanvasConnectionMode.Output,
    type,
    index: parseInt(indexStr, 10),
  }
}

/**
 * Insert spacer entries between endpoints for better layout
 */
export function insertSpacersBetweenEndpoints(endpoints, requiredCount) {
  if (endpoints.length <= 1) return endpoints

  const result = []
  for (let i = 0; i < endpoints.length; i++) {
    result.push(endpoints[i])
    // Add spacer after each endpoint except the last one
    if (i < endpoints.length - 1) {
      result.push(null) // null acts as spacer
    }
  }
  return result
}

/**
 * Calculate the node size based on inputs/outputs and configuration
 */
export function calculateNodeSize(
  isConfiguration = false,
  isConfigurable = false,
  mainInputsCount = 1,
  mainOutputsCount = 1,
  nonMainInputsCount = 0,
  isExperimentalNdvActive = false
) {
  const baseWidth = isConfiguration ? 50 : 100
  const baseHeight = isConfiguration ? 50 : 80

  // Configuration nodes are circular
  if (isConfiguration) {
    return { width: 50, height: 50 }
  }

  // Configurable nodes are wider
  if (isConfigurable) {
    return { width: 240, height: 72 }
  }

  // Standard nodes
  return { width: 100, height: 80 }
}

/**
 * Get mouse position from event
 */
export function getMousePosition(event) {
  if ('clientX' in event) {
    return [event.clientX, event.clientY]
  }
  if ('touches' in event && event.touches.length > 0) {
    return [event.touches[0].clientX, event.touches[0].clientY]
  }
  return [0, 0]
}

/**
 * Check if a value is present (not null or undefined)
 */
export function isPresent(value) {
  return value !== null && value !== undefined
}

/**
 * Create a unique ID for canvas elements
 */
export function createCanvasId(prefix = 'canvas') {
  return `${prefix}_${Math.random().toString(36).substr(2, 9)}`
}

/**
 * Debounce function
 */
export function debounce(fn, delay) {
  let timeoutId
  return (...args) => {
    clearTimeout(timeoutId)
    timeoutId = setTimeout(() => fn(...args), delay)
  }
}

/**
 * Throttle function
 */
export function throttle(fn, limit) {
  let inThrottle
  return (...args) => {
    if (!inThrottle) {
      fn(...args)
      inThrottle = true
      setTimeout(() => (inThrottle = false), limit)
    }
  }
}
