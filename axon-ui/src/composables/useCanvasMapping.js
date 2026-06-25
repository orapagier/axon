
/**
 * useCanvasMapping composable - maps Axon workflow data to Vue Flow format
 * Inspired by n8n's canvas-mapping approach; independent implementation.
 */

import { computed } from 'vue'
import { NodeConnectionTypes, CanvasConnectionMode } from '../lib/canvas/constants.js'
import { getNodeOutputs } from '../lib/nodes.js'

/**
 * Map Axon nodes to Vue Flow nodes with n8n-style data structure
 */
export function useCanvasMapping({ nodes, edges, workflowObject }) {
  // Map nodes to Vue Flow format
  const mappedNodes = computed(() => {
    return nodes.value.map((node) => {
      const data = node.data || {}

      return {
        id: node.id,
        type: 'canvas-node', // Use our custom canvas-node type
        position: node.position || { x: 0, y: 0 },
        data: {
          // Node identity
          name: data.name || data.label || node.id,
          label: data.label || data.name,
          type: data.node_type || node.type,

          // Node state
          disabled: data.disabled || false,
          enabled: data.enabled !== false,

          // Node rendering options
          render: data.render || {
            type: 'default',
            options: {
              trigger: data.render?.options?.trigger || data.node_type === 'trigger',
              icon: data.render?.options?.icon || getNodeIcon(data.node_type || node.type),
            },
          },

          // Execution state
          execution: data.execution || {
            running: false,
            waiting: false,
            status: 'unknown',
          },

          // Connections configuration
          inputs: data.inputs || (data.node_type === 'trigger' ? [] : [
            { type: NodeConnectionTypes.Main, required: true },
          ]),
          outputs: data.outputs || [
            { type: NodeConnectionTypes.Main, required: false },
          ],
          connections: data.connections || { input: {}, output: {} },

          // Node configuration
          config: data.config || {},

          // Additional metadata
          ...data,
        },
      }
    })
  })

  // Map edges to Vue Flow format with proper data structure
  const mappedEdges = computed(() => {
    return edges.value.map((edge) => {
      // Parse handle names to determine connection types
      const sourceHandle = edge.sourceHandle || `output_${NodeConnectionTypes.Main}_0`
      const targetHandle = edge.targetHandle || `input_${NodeConnectionTypes.Main}_0`

      return {
        id: edge.id,
        source: edge.source,
        target: edge.target,
        sourceHandle,
        targetHandle,
        type: 'canvas-edge',
        animated: edge.data?.animated || false,
        data: {
          // Connection data for edge rendering
          source: edge.source,
          target: edge.target,
          sourceHandle,
          targetHandle,
          // Connection type info
          sourceType: parseHandleType(sourceHandle),
          targetType: parseHandleType(targetHandle),
          // Status
          status: edge.data?.status,
          // Whether this is a "main" connection or configuration
          isMain: sourceHandle.includes(NodeConnectionTypes.Main) && targetHandle.includes(NodeConnectionTypes.Main),
          ...edge.data,
        },
      }
    })
  })

  return {
    nodes: mappedNodes,
    edges: mappedEdges,
  }
}

/**
 * Get icon for node type
 */
function getNodeIcon(nodeType) {
  const iconMap = {
    trigger: '⚡',
    synapse: '🌐',
    myelin: '🛡️',
    shell: '💻',
    javascript: '📜',
    python: '🐍',
    sql: '🗄️',
    email: '📧',
    file: '📁',
    ifCondition: '🔀',
    switch: '🧭',
    condition: '🔀',
    loop: '🔄',
    wait: '⏱️',
    delay: '⏱️',
    webhook: '🔔',
    schedule: '📅',
    manual: '👆',
    default: '📦',
  }
  return iconMap[nodeType] || iconMap.default
}

/**
 * Parse connection type from handle ID
 * Handle format: "input_main_0" or "output_error_0"
 */
function parseHandleType(handleId) {
  if (!handleId) return NodeConnectionTypes.Main

  const parts = handleId.split('_')
  if (parts.length >= 2) {
    const type = parts[1]
    // Map common type names
    if (Object.values(NodeConnectionTypes).includes(type)) {
      return type
    }
  }
  return NodeConnectionTypes.Main
}

/**
 * Create initial node data structure
 */
export function createNodeData(id, type, name, config = {}, enabled = true) {
  const isTrigger = type === 'trigger'
  // Output handles: fixed for most nodes, but config-derived for dynamic ones
  // like Switch (one handle per rule + optional Default).
  const customOutputs = getNodeOutputs(type, config) // e.g. ['true', 'false'] for IF node

  const outputs = customOutputs
    ? customOutputs.map((label, i) => ({
      type: NodeConnectionTypes.Main,
      required: false,
      index: i,
      label,
    }))
    : [{ type: NodeConnectionTypes.Main, required: false }]

  return {
    id,
    type: 'canvas-node',
    name,
    label: name,
    node_type: type,
    config,
    enabled,
    disabled: !enabled,
    inputs: isTrigger ? [] : [
      { type: NodeConnectionTypes.Main, required: true },
    ],
    outputs,
    connections: {
      input: {},
      output: {},
    },
    execution: {
      running: false,
      waiting: false,
      status: 'unknown',
    },
    render: {
      type: 'default',
      options: {
        trigger: isTrigger,
        icon: getNodeIcon(type),
      },
    },
  }
}

/**
 * Create edge data structure
 */
export function createEdgeData(id, source, target, sourceHandle, targetHandle) {
  return {
    id,
    source,
    target,
    sourceHandle: sourceHandle || `output_${NodeConnectionTypes.Main}_0`,
    targetHandle: targetHandle || `input_${NodeConnectionTypes.Main}_0`,
    type: 'canvas-edge',
    data: {
      source,
      target,
      sourceHandle,
      targetHandle,
    },
  }
}
