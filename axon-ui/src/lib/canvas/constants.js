/**
 * Canvas types and enums; architecture inspired by n8n, independent implementation
 */

// Node render types
export const CanvasNodeRenderType = {
  Default: 'default',
  AddNodes: 'addNodes',
  StickyNote: 'stickyNote',
  ChoicePrompt: 'choicePrompt',
}

// Connection modes
export const CanvasConnectionMode = {
  Input: 'input',
  Output: 'output',
}

// Connection types
export const NodeConnectionTypes = {
  Main: 'main',
  AiTool: 'ai_tool',
  AiDocument: 'ai_document',
  AiEmbedding: 'ai_embedding',
  AiMemory: 'ai_memory',
  AiModel: 'ai_model',
  AiOutputParser: 'ai_outputParser',
  AiRetriever: 'ai_retriever',
  AiTextSplitter: 'ai_textSplitter',
  AiVectorStore: 'ai_vectorStore',
}

// Execution statuses
export const CanvasNodeExecutionStatus = {
  Running: 'running',
  Waiting: 'waiting',
  WaitingForNext: 'waitingForNext',
  Success: 'success',
  Error: 'error',
  Unknown: 'unknown',
}

// Grid size for snapping
export const GRID_SIZE = 20

// Configuration node radius
export const CONFIGURATION_NODE_RADIUS = 25

// Canvas injection key
export const CanvasKey = Symbol('canvas')

// Canvas node injection key
export const CanvasNodeKey = Symbol('canvas-node')
