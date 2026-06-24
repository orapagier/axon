/**
 * useCanvas composable - provides shared canvas state
 * Inspired by n8n's canvas-state pattern; independent implementation.
 */

import { inject, provide, ref, computed } from 'vue'
import { CanvasKey } from '../lib/canvas/constants.js'

/**
 * Provide canvas context to child components
 */
export function provideCanvas(context) {
  provide(CanvasKey, context)
  return context
}

/**
 * Inject canvas context from parent
 */
export function useCanvas() {
  const context = inject(CanvasKey, null)

  if (!context) {
    // Return default values if no context provided
    return {
      connectingHandle: ref(null),
      isExecuting: ref(false),
      initialized: ref(false),
      viewport: ref({ x: 0, y: 0, zoom: 1 }),
      isPaneMoving: ref(false),
      isExperimentalNdvActive: ref(false),
    }
  }

  return context
}

/**
 * Create canvas context
 */
export function createCanvasContext() {
  return {
    connectingHandle: ref(null),
    isExecuting: ref(false),
    initialized: ref(false),
    viewport: ref({ x: 0, y: 0, zoom: 1 }),
    isPaneMoving: ref(false),
    isExperimentalNdvActive: ref(false),
  }
}
