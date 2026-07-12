// Exports a trimmed, LLM-oriented copy of the workflow node palette
// (NODE_TYPES in src/lib/nodes.js) to crates/axon-agent/assets/node_types.json,
// where it is embedded via include_str! and served by the agent's
// list_node_types internal tool. Re-run whenever nodes.js changes:
//   node scripts/export-node-types.mjs   (from axon-ui/)
// The output is committed so cargo builds never depend on a prior UI build.
import { writeFileSync, mkdirSync } from 'node:fs'
import { dirname, join } from 'node:path'
import { fileURLToPath } from 'node:url'
import { NODE_TYPES } from '../src/lib/nodes.js'

// UI-only property fields the agent doesn't need to emit valid config.
const DROP_PROP_FIELDS = new Set(['placeholder', 'noExpr', 'searchable', 'allowCustomValue'])

function trimProperty(prop) {
    // Pure UI banners — nothing for the agent to fill in.
    if (prop.type === 'notice') return null
    const out = {}
    for (const [k, v] of Object.entries(prop)) {
        if (DROP_PROP_FIELDS.has(k)) continue
        if (k === 'typeOptions') {
            // Keep only semantically meaningful typeOptions (row counts etc. are UI).
            if (v && v.multipleValues) out.typeOptions = { multipleValues: true }
            continue
        }
        out[k] = v
    }
    return out
}

const trimmed = {}
for (const [key, def] of Object.entries(NODE_TYPES)) {
    // The static `mcp` key is a placeholder replaced at runtime by mcp_<service>
    // node types; agents discover MCP tools via the tool registry instead.
    if (key === 'mcp') continue
    const entry = {
        displayName: def.displayName,
        description: def.description || '',
        properties: (def.properties || []).map(trimProperty).filter(Boolean),
    }
    if (def.inputs !== undefined) entry.inputs = def.inputs
    if (def.outputs !== undefined) entry.outputs = def.outputs
    if (def.dynamicOutputs) entry.dynamicOutputs = true
    trimmed[key] = entry
}

const here = dirname(fileURLToPath(import.meta.url))
const outPath = join(here, '..', '..', 'crates', 'axon-agent', 'assets', 'node_types.json')
mkdirSync(dirname(outPath), { recursive: true })
writeFileSync(outPath, JSON.stringify(trimmed, null, 1) + '\n')
console.log(`Wrote ${Object.keys(trimmed).length} node types to ${outPath}`)
