<script setup>
import { computed, nextTick, onBeforeUnmount, ref, watch } from 'vue'
import { useHeaderSearch } from '../lib/headerSearch.js'

const DOC_SECTIONS = [
  {
    id: 'platform-overview',
    category: 'Foundations',
    title: 'Platform Overview',
    summary: 'How Axon is structured and what each major module is responsible for.',
    tags: ['overview', 'architecture', 'dashboard', 'agent'],
    blocks: [
      {
        type: 'paragraph',
        text: 'Axon is a single-page operations dashboard for running an AI agent platform. The UI is built with Vue 3 and Vite, then organized into feature-focused pages inside one shell.',
      },
      {
        type: 'bullet',
        title: 'Core design principles',
        items: [
          'One command center shell with fast page switching and a persistent top bar.',
          'Realtime-first interactions in Chat and Workflow execution.',
          'Configuration pages that map directly to backend features and API endpoints.',
          'Composable automation stack: models, tools, memories, tasks, workflows, and services.',
        ],
      },
      {
        type: 'table',
        title: 'Primary modules',
        columns: ['Module', 'Primary Purpose', 'Typical Owner'],
        rows: [
          ['Chat', 'Run agent conversations and observe tool traces live.', 'Operators'],
          ['Models', 'Control model providers, priorities, quotas, and fallback.', 'AI Platform'],
          ['Workflows', 'Build node-based automations and execute orchestration graphs.', 'Automation Engineers'],
          ['Services', 'Manage integrations, auth connectors, credentials, and account wiring.', 'Infrastructure'],
          ['Settings', 'Tune runtime behavior and routing patterns.', 'Admins'],
        ],
      },
    ],
  },
  {
    id: 'access-and-sessions',
    category: 'Foundations',
    title: 'Access, Authentication, and Session Lifecycle',
    summary: 'How users enter the app and how authorization is applied to API and websocket traffic.',
    tags: ['auth', 'security', 'session', 'master key'],
    blocks: [
      {
        type: 'paragraph',
        text: 'The dashboard is protected by a master key flow. Once authenticated, the key is stored in localStorage as AXON_MASTER_KEY and attached to outbound requests.',
      },
      {
        type: 'bullet',
        title: 'Authentication flow',
        items: [
          'Unauthenticated users are sent to Login view before any app modules are rendered.',
          'API helper automatically sends Authorization: Bearer <AXON_MASTER_KEY> for JSON calls.',
          '401 responses trigger key removal and force a clean reload to return to Login.',
          'Websocket stream also receives the key via query string for live events.',
        ],
      },
      {
        type: 'note',
        text: 'Security recommendation: rotate the master key regularly and avoid sharing browser profiles across users.',
      },
    ],
  },
  {
    id: 'navigation-shell',
    category: 'Foundations',
    title: 'Navigation Shell and Layout',
    summary: 'Behavior of the global sidebar, top bar status area, page host, and responsive behavior.',
    tags: ['navigation', 'layout', 'sidebar', 'responsive'],
    blocks: [
      {
        type: 'paragraph',
        text: 'The app shell handles module navigation without full page reloads. On smaller screens, the sidebar becomes a drawer and closes automatically after navigation.',
      },
      {
        type: 'bullet',
        title: 'Shell behaviors',
        items: [
          'Sidebar can be collapsed or expanded on desktop for compact operations.',
          'Top bar displays current module context and websocket connection health.',
          'Main content host renders one active page component at a time.',
          'Global refresh and logout actions are available from the top-right controls.',
        ],
      },
      {
        type: 'code',
        title: 'Page registration pattern',
        language: 'js',
        code: "const PAGES = {\n  chat: defineAsyncComponent(() => import('./pages/ChatPage.vue')),\n  docs: defineAsyncComponent(() => import('./pages/DocsPage.vue')),\n  settings: defineAsyncComponent(() => import('./pages/SettingsPage.vue')),\n}",
      },
    ],
  },
  {
    id: 'chat-and-runtime-stream',
    category: 'Chat',
    title: 'Chat Runtime and Live Event Stream',
    summary: 'How user messages, tool traces, tokens, and final responses are streamed into the chat UI.',
    tags: ['chat', 'websocket', 'tokens', 'trace', 'streaming'],
    blocks: [
      {
        type: 'paragraph',
        text: 'The Chat page opens a persistent websocket session and receives execution events in realtime. Users see progressive status updates, tool call traces, and token-level streaming output.',
      },
      {
        type: 'table',
        title: 'Runtime event mapping',
        columns: ['Event Type', 'What It Represents', 'UI Effect'],
        rows: [
          ['thinking', 'Planner or intermediate reasoning step.', 'Updates in-flight status label.'],
          ['tools', 'Tool tier and tool list selected for this iteration.', 'Appends routing details to trace box.'],
          ['tool_start', 'A specific tool call has started.', 'Trace entry appears as running.'],
          ['tool_end', 'Tool call completed with success/failure.', 'Trace line resolves with timing and state.'],
          ['token', 'Partial model text response token chunk.', 'Agent bubble grows live as text streams.'],
          ['done', 'Run is complete with final metadata.', 'Run closes and chat input is re-enabled.'],
        ],
      },
      {
        type: 'bullet',
        title: 'Operator controls',
        items: [
          'Starter prompts accelerate common asks for new sessions.',
          'Enter submits, Shift+Enter inserts a new line.',
          'Trace panel records execution metadata for auditability.',
          'Input auto-focus behavior keeps keyboard-first workflows fast.',
        ],
      },
    ],
  },
  {
    id: 'models-page',
    category: 'Control Planes',
    title: 'Models Page (Provider and Capacity Control)',
    summary: 'Manage model providers, routing roles, quotas, status, and failover readiness.',
    tags: ['models', 'providers', 'rate limit', 'quota', 'routing'],
    blocks: [
      {
        type: 'paragraph',
        text: 'The Models module is the control plane for all model backends used by Axon. It supports provider setup, priority ordering, role assignment, and bulk enable/disable actions.',
      },
      {
        type: 'bullet',
        title: 'What you can configure',
        items: [
          'Provider, model ID, API key, optional base URL, and max tokens.',
          'Priority and role tags for routing behavior (for example router or fallback tiers).',
          'Enable/disable controls per model and in bulk.',
          'Reset actions for unhealthy models with consecutive errors or degraded status.',
        ],
      },
      {
        type: 'table',
        title: 'Provider support in UI',
        columns: ['Provider', 'Use Case', 'Typical Configuration'],
        rows: [
          ['OpenAI', 'General high-quality assistant tasks.', 'model_id + API key'],
          ['Anthropic', 'Long-form reasoning and analytical tasks.', 'model_id + API key'],
          ['Google Gemini', 'Multi-modal or broad utility routing.', 'model_id + API key'],
          ['Ollama', 'Local/self-hosted runs.', 'model_id + base_url'],
          ['OpenRouter', 'Brokered multi-provider fallback.', 'model_id + API key'],
        ],
      },
    ],
  },
  {
    id: 'tools-page',
    category: 'Control Planes',
    title: 'Tools Page (Execution Surface)',
    summary: 'Inventory and governance of callable tools grouped by source type.',
    tags: ['tools', 'mcp', 'internal', 'enable disable', 'reload'],
    blocks: [
      {
        type: 'paragraph',
        text: 'The Tools module lists all callable tools that the runtime can invoke. Entries are grouped by source, such as internal registries or external MCP sources.',
      },
      {
        type: 'bullet',
        title: 'Common operations',
        items: [
          'Reload tools from disk-backed directories.',
          'Enable or disable specific tools without server restarts.',
          'Review required argument fields before making a tool available.',
          'Compare tool counts by source to validate deployment state.',
        ],
      },
      {
        type: 'note',
        text: 'If a tool does not appear here after deployment, use Reload Tools first, then confirm that the backend tool registry endpoint is healthy.',
      },
    ],
  },
  {
    id: 'memories-page',
    category: 'State and Context',
    title: 'Memories Page (Short-Term and Long-Term Context)',
    summary: 'Inspect execution history, run traces, and searchable persistent memory records.',
    tags: ['memory', 'runs', 'trace', 'search', 'ltm', 'stm'],
    blocks: [
      {
        type: 'paragraph',
        text: 'Memories is split into Short-Term Memory (run history and execution details) and Long-Term Memory (retrievable knowledge snippets). It is the best view for post-run analysis.',
      },
      {
        type: 'bullet',
        title: 'Short-Term Memory includes',
        items: [
          'Run-level metadata: iterations, tokens, tools, models, and output.',
          'Expandable execution details with tool call arguments, result payloads, and errors.',
          'Human-readable timing labels for triage and troubleshooting.',
        ],
      },
      {
        type: 'bullet',
        title: 'Long-Term Memory includes',
        items: [
          'Recent memory feed with source and created-at metadata.',
          'Semantic search endpoint with top_k retrieval.',
          'Record-level delete controls for memory hygiene.',
        ],
      },
    ],
  },
  {
    id: 'tasks-page',
    category: 'Automation',
    title: 'Tasks Page (Scheduled Jobs)',
    summary: 'Automate recurring work with cron-based scheduled jobs.',
    tags: ['tasks', 'scheduler', 'jobs', 'cron'],
    blocks: [
      {
        type: 'paragraph',
        text: 'Tasks is focused on scheduled jobs. Define a cron schedule, attach an instruction, and let the scheduler execute your automation on time.',
      },
      {
        type: 'table',
        title: 'Automation modes',
        columns: ['Mode', 'Trigger Pattern', 'Typical Example'],
        rows: [
          ['Job', 'Cron expression with optional stop condition.', 'Daily executive summary at 09:00.'],
          ['Custom Cron Job', 'Manual cron expression input.', 'Run every 15 minutes during business hours.'],
        ],
      },
      {
        type: 'bullet',
        title: 'Operational controls',
        items: [
          'Pause/resume individual jobs.',
          'Run now for immediate execution testing.',
          'Edit schedule modes through a structured builder.',
          'Track run count and last-run timestamps from the jobs list.',
        ],
      },
    ],
  },
  {
    id: 'workflows-page',
    category: 'Automation',
    title: 'Workflows Canvas (Graph Automation Engine)',
    summary: 'Design, run, and monitor node-based workflows with live playback and run history.',
    tags: ['workflows', 'canvas', 'nodes', 'mcp', 'execution', 'history'],
    blocks: [
      {
        type: 'paragraph',
        text: 'Workflows is the visual automation engine. You can create node graphs, map connections, execute full runs or individual steps, and inspect run-by-run output history.',
      },
      {
        type: 'bullet',
        title: 'Key capabilities',
        items: [
          'Node picker with search and dynamic schemas for MCP tools.',
          'Per-node execution state tracking (running, waiting, success, error).',
          'Step execution and full-run execution from the same editor.',
          'Run polling against workflow-runs endpoint for lightweight live updates.',
          'Copy/paste graph fragments for rapid composition.',
        ],
      },
      {
        type: 'table',
        title: 'Representative node families',
        columns: ['Node Family', 'Role in Graph', 'Examples'],
        rows: [
          ['Stimulus', 'Trigger and entrypoint behavior.', 'Manual, cron, Telegram, Gmail trigger modes'],
          ['Synapse', 'HTTP and integration calls.', 'REST requests, auth headers, query/body payloads'],
          ['MCP Nodes', 'External tool execution with generated fields.', 'Google tools, CRM tools, custom MCP servers'],
          ['Myelin/Fovea', 'Storage and media processing.', 'Save/retrieve files, visual processing flows'],
        ],
      },
    ],
  },
  {
    id: 'workflow-lineage-and-new-nodes',
    category: 'Automation',
    title: 'Per-Item Lineage and n8n Parity Nodes',
    summary: 'For-each execution with $ancestor lineage, plus GraphQL, Compare Datasets, Python, SSH, FTP/FTPS, JWT/TOTP/signature/encrypt crypto, and cross-run dedupe.',
    tags: ['workflows', 'for_each', 'lineage', 'ancestor', 'graphql', 'python', 'ssh', 'ftp', 'ftps', 'jwt', 'totp', 'sign', 'signature', 'dedupe', 'compare', 'n8n'],
    blocks: [
      {
        type: 'paragraph',
        text: 'These capabilities close the remaining gaps against n8n\'s core node set — and in several places go past it. Everything is opt-in: existing workflows behave exactly as before.',
      },
      {
        type: 'bullet',
        title: 'Per-item lineage ("For Each Item")',
        items: [
          'Any request/transform node has a "For Each Item" toggle in its Settings tab: it maps over the input array, binding $item / $index and (for these) $json to the current item — no explicit Loop node.',
          'Parallelism knob runs items concurrently (a real edge over n8n\'s single-threaded per-item loop); Stamp Lineage Index adds __idx to each output.',
          '$ancestor("Node Name") in any expression resolves to the SPECIFIC upstream item this one came from — by the __idx stamp if present, otherwise by position. It is n8n\'s paired-item mapping without the fragile "can\'t determine which item" failures, because lineage is explicit.',
          'Split Out has a matching "Stamp Lineage Index" option so a downstream $ancestor() still joins correctly after Filter/Sort reshuffle the list.',
        ],
      },
      {
        type: 'table',
        title: 'New nodes',
        columns: ['Node', 'What it does', 'Beyond n8n'],
        rows: [
          ['GraphQL', 'Query/mutation with variables, operationName, every HTTP auth mode.', 'Treats the errors[] array in an HTTP-200 body as a real failure.'],
          ['Compare Datasets', 'Diff two lists by key into same / different / a_only / b_only + counts.', 'One composable output object instead of four forced branches.'],
          ['Python', 'Runs real CPython on the host with _json / _item / _node / _results globals.', 'Full native packages (pandas, requests) — n8n\'s Python is a WASM sandbox.'],
          ['SSH', 'Remote command + SFTP upload/download against a saved server.', 'Reuses the agent\'s host-key-pinned SSH stack and destructive-command guards.'],
          ['FTP', 'List / download / upload / delete / rename / mkdir over FTP or explicit FTPS, inline or saved credentials.', 'Self-signed-cert tolerance is a per-node toggle; List emits one item per file, Loop-ready.'],
        ],
      },
      {
        type: 'table',
        title: 'Extended nodes',
        columns: ['Node', 'New operations / options', 'Use'],
        rows: [
          ['Crypto', 'JWT sign & verify (HS/RS/ES/PS/EdDSA), TOTP generate & verify, AES-256-GCM encrypt/decrypt.', 'Mint/check API tokens, 2FA codes, encrypt a field at rest — was hash/HMAC/UUID only.'],
          ['Crypto — Signatures', 'Raw Sign / Verify over any value with RSA (RS/PS), ECDSA (ES256/384), Ed25519, or HMAC — hex/base64/base64url output, auto-detected on verify.', 'Sign API payloads or verify provider signatures without a JWT wrapper (n8n signs but cannot verify).'],
          ['Sort / Limit / Dedupe', 'Dedupe Scope "Across all runs" (persistent), with a per-node key cap and reset.', 'Pair with a polling trigger for "only new items"; test runs check but never record.'],
        ],
      },
      {
        type: 'note',
        text: 'Cross-run dedupe records seen-keys only on real full runs — manual/step runs in the editor check the memory read-only, so experimenting never eats real events. This is stricter (and safer) than n8n, whose manual runs pollute its dedupe store.',
      },
      {
        type: 'code',
        title: 'Example: per-item enrichment with lineage',
        language: 'text',
        code: "[Webhook: 3 orders]\n  -> [Synapse: shipping quote]   For Each Item = on, Stamp Lineage Index = on\n  -> [Soma: compose message]     For Each Item = on\n         text = \"Hi {{ $ancestor('Webhook').customer }}, \" +\n                \"your quote is {{ $json.quote }}\"\n\nEach message automatically gets the customer from the SAME order that\nproduced its quote — no manual field carrying, no Merge gymnastics.",
      },
    ],
  },
  {
    id: 'crm-data-model',
    category: 'CRM',
    title: 'CRM Data Model — Orgs, Leads, Deals, Activities',
    summary: 'How Organizations, Leads, Deals, and Activities relate, and why they do not overlap.',
    tags: ['crm', 'leads', 'deals', 'orgs', 'organizations', 'data model', 'activities'],
    blocks: [
      {
        type: 'paragraph',
        text: 'Axon ships a built-in, SQLite-backed CRM exposing 34 tools across five domains: Leads, Deals, Organizations, Activities, and Insights. Data lives in a single crm.db file. Each entity answers a different question, so they are layers, not overlapping alternatives.',
      },
      {
        type: 'code',
        title: 'Entity hierarchy',
        language: 'text',
        code: "Organizations (orgs)\n    +-- Leads  (org_id -> orgs.id, optional)\n    |     +-- Deals  (contact_id -> leads.id, required)\n    |               +-- Activities (entity_type = 'deal')\n    +-- Activities (entity_type = 'org')\n    +-- Leads -> Activities (entity_type = 'lead')",
      },
      {
        type: 'table',
        title: 'What each entity answers',
        columns: ['Entity', 'Answers the question...', 'Required parent', 'Key fields'],
        rows: [
          ['Organization', 'Which company or account?', 'None', 'name, website, industry, size, country'],
          ['Lead', 'Which person, and how engaged are they?', 'None (org_id optional)', 'name, email, phone, status: Open/Contacted/Qualified/Lost, source'],
          ['Deal', 'How much money, what stage, closing when?', 'Lead required (contact_id); org optional', 'title, amount, currency, stage, probability, expected_close'],
          ['Activity', 'What happened, and when?', 'Any lead, deal, or org', 'kind: note/call/email/meeting/task, title, body, occurred_at'],
        ],
      },
      {
        type: 'note',
        text: "They do not overlap: an Organization has no status or money, a Lead has no monetary value, and a Deal borrows its contact info from its Lead rather than storing its own. The one field that looks redundant, Lead.company, is deliberately free text (e.g. \"works at Acme\") and is NOT linked to anything; org_id is the real relational link you set once you actually want that company tracked as an account with its own deals and other contacts.",
      },
      {
        type: 'bullet',
        title: 'Deletion, archive, and conversion rules',
        items: [
          'Deleting a lead is blocked if it has active deals — archive it instead, or remove the deals first.',
          'Deleting an org is blocked if it has any active leads or deals.',
          'Archive (crm_record_archive) is reversible and hides a record from normal queries; hard delete requires confirm_permanent: true and cascades its activities.',
          'crm_lead_convert_to_deal is the recommended prospecting-to-sales path: one atomic call creates the Deal and advances the Lead\'s status (default Qualified).',
        ],
      },
    ],
  },
  {
    id: 'crm-tool-reference',
    category: 'CRM',
    title: 'CRM Tool Reference (34 tools across 5 domains)',
    summary: 'Every crm_* tool grouped by domain, with the fields and guardrails that matter.',
    tags: ['crm', 'tools', 'mcp', 'reference', 'leads', 'deals', 'orgs', 'activities'],
    blocks: [
      {
        type: 'paragraph',
        text: 'Workflow nodes can call every CRM tool. The chat agent gets read tools by default (list/get/search/overview/pipeline/dashboard/change-feed/export/backup); write tools (create/update/delete/convert/archive/restore) are off by default and gated per-tool on the Tools page — flip a tool\'s Enable toggle to grant the chat agent that specific write action. Applies immediately, no restart needed.',
      },
      {
        type: 'table',
        title: 'Leads',
        columns: ['Tool', 'Purpose', 'Notes'],
        rows: [
          ['crm_lead_create', 'Create a lead', 'Only name is required; duplicate-guarded by email'],
          ['crm_lead_list / _get / _search', 'Read leads', 'Search covers name, email, company, notes, tags'],
          ['crm_lead_update', 'Patch fields', 'null clears a field; an omitted field is left unchanged'],
          ['crm_lead_delete', 'Hard delete', 'Blocked if active deals reference it; prefer archive'],
          ['crm_lead_convert_to_deal', 'Lead -> Deal in one transaction', 'Also advances the lead\'s status (default Qualified)'],
        ],
      },
      {
        type: 'table',
        title: 'Deals',
        columns: ['Tool', 'Purpose', 'Notes'],
        rows: [
          ['crm_deal_create', 'Create a deal', 'Requires title + contact_id (an existing, active lead)'],
          ['crm_deal_list / _get / _search', 'Read deals', 'list returns total_value per currency, never summed across currencies'],
          ['crm_deal_update', 'Patch fields, including stage', '"probability": null clears it explicitly; omitted keeps the current value'],
          ['crm_deal_delete', 'Hard delete', 'Requires confirm_permanent; prefer archive'],
          ['crm_pipeline_summary', 'Pipeline by stage', 'Includes win_rate_pct = Won / (Won + Lost)'],
        ],
      },
      {
        type: 'table',
        title: 'Organizations',
        columns: ['Tool', 'Purpose', 'Notes'],
        rows: [
          ['crm_org_create', 'Create an org', 'Only name is required; duplicate-guarded by name'],
          ['crm_org_list / _get / _search', 'Read orgs', 'Search covers name, industry, country, website, notes, tags'],
          ['crm_org_update / _delete', 'Patch or hard delete', 'Delete blocked if active leads or deals reference it'],
        ],
      },
      {
        type: 'table',
        title: 'Activities',
        columns: ['Tool', 'Purpose', 'Notes'],
        rows: [
          ['crm_activity_log', 'Log a note, call, email, meeting, or task', 'Attaches to any lead, deal, or org'],
          ['crm_activity_list / _get', 'Read activities', 'entity_id and entity_type filter independently'],
          ['crm_activity_update', 'Patch, or reassign to a different entity', 'entity_id + entity_type must be provided together when reassigning'],
          ['crm_activity_delete', 'Hard delete', 'Requires confirm_permanent'],
        ],
      },
      {
        type: 'table',
        title: 'Insights & Ops',
        columns: ['Tool', 'Purpose', 'Notes'],
        rows: [
          ['crm_search_all', 'Search orgs + leads + deals in one call', 'Activities are not included — use crm_activity_list'],
          ['crm_record_overview', 'Full 360-degree view of one record', 'Plus linked records and recent activities'],
          ['crm_dashboard_summary', 'Daily operational snapshot', 'Stale leads/deals, closing-soon deals, weighted pipeline'],
          ['crm_record_archive / _restore / _archived_list', 'Soft-delete lifecycle', 'Restore re-validates that parent records are still active'],
          ['crm_export_snapshot / crm_backup_db', 'Backup and export', 'Writes to the Files page; backup restores by swapping crm.db'],
          ['crm_changes_since', 'Change feed since a cursor', 'Powers the CRM workflow trigger (next section)'],
        ],
      },
    ],
  },
  {
    id: 'crm-workflow-trigger',
    category: 'CRM',
    title: 'CRM Workflow Trigger — Polling, Not Push',
    summary: 'The CRM Stimulus trigger watches the database on an interval; nothing pushes events into it from outside.',
    tags: ['crm', 'trigger', 'stimulus', 'workflow', 'polling', 'webhook', 'push'],
    blocks: [
      {
        type: 'paragraph',
        text: 'The Stimulus node\'s CRM trigger type is the "when something happens in the CRM, run this workflow" entry point, but it is not an inbound webhook. A background loop calls crm_changes_since on the configured interval and diffs updated_at timestamps against a stored cursor; whatever changed since the last poll becomes the "event". This is the same poll-first pattern used for the Gmail trigger.',
      },
      {
        type: 'table',
        title: 'Push vs. poll trigger types in Axon',
        columns: ['Trigger Type', 'Delivery', 'How it arrives'],
        rows: [
          ['Facebook', 'Push', 'Meta calls /webhook/facebook the instant an event happens'],
          ['Telegram', 'Push', 'Bot API calls /webhook/telegram the instant a message arrives'],
          ['WhatsApp', 'Push', 'Meta calls /webhook/whatsapp the instant a message arrives'],
          ['GitHub', 'Push', 'Repo calls /webhook/github/:workflow_id on repo events'],
          ['Webhook (generic)', 'Push', 'Any caller POSTs /webhook/external/:workflow_id'],
          ['Gmail', 'Poll', 'Background loop checks for new mail on the configured interval'],
          ['CRM', 'Poll', 'Background loop diffs crm_changes_since against a stored cursor'],
        ],
      },
      {
        type: 'table',
        title: 'Configuration (Stimulus -> Trigger Type: CRM)',
        columns: ['Field', 'Values', 'Notes'],
        rows: [
          ['CRM Event', 'New Lead / New Deal / Deal Stage Changed / Any Change', 'Filters which changes count as a hit'],
          ['Poll Interval', 'Minutes (default 5)', 'Only affects reaction latency — the record already exists the instant it is created'],
        ],
      },
      {
        type: 'code',
        title: 'Trigger output (one run per poll batch)',
        language: 'json',
        code: "{\n  \"trigger\": \"crm\",\n  \"event\": \"deal_stage_changed\",\n  \"change_count\": 1,\n  \"changes\": [\n    { \"entity_type\": \"deal\", \"change\": \"updated\", \"id\": \"...\", \"title\": \"...\",\n      \"stage\": \"Won\", \"previous_stage\": \"Negotiation\", \"amount\": 12500.0 }\n  ]\n}",
      },
      {
        type: 'bullet',
        title: 'Semantics worth knowing',
        items: [
          'First poll is a silent baseline — it stores the cursor (and current deal stages) without firing; only changes from then on fire.',
          'Deal Stage Changed fires only on an actual stage move (previous_stage is null the first time the trigger sees a deal — it records silently and fires from that deal\'s next change).',
          'A merely-edited lead (notes, phone) is tagged "updated", not "created" — New Lead only fires on real creates.',
          'The cursor advances before the run fires, so a crash mid-run cannot re-fire the same batch forever.',
        ],
      },
      {
        type: 'note',
        text: 'Because this is a poll, "New Lead" only fires for rows that reach crm_lead_create — it cannot see a Facebook message or a webhook body directly. Whatever channel is capturing the lead must call crm_lead_create itself first. See the Example Flows section below for Facebook, website-webhook, and email capture feeding into this trigger.',
      },
    ],
  },
  {
    id: 'crm-example-flows',
    category: 'CRM',
    title: 'CRM Example Flows',
    summary: 'Step-by-step chat sequences and full automations, including capturing leads from Facebook, a website form, and email.',
    tags: ['crm', 'examples', 'automation', 'facebook', 'webhook', 'gmail', 'flows'],
    blocks: [
      {
        type: 'code',
        title: 'Flow 1: New Lead -> Deal -> Close (chat / agent sequence)',
        language: 'text',
        code: "1. crm_org_create           -> Create the company account\n2. crm_lead_create          -> Add the contact, link to org\n3. crm_activity_log         -> Log initial call (kind: \"call\")\n4. crm_lead_update          -> Advance status to \"Contacted\" or \"Qualified\"\n5. crm_lead_convert_to_deal -> Create deal, auto-update lead status\n6. crm_activity_log         -> Log proposal meeting (kind: \"meeting\")\n7. crm_deal_update          -> Advance stage -> \"Proposal\" -> \"Negotiation\"\n8. crm_deal_update          -> stage: \"Won\", probability: 100",
      },
      {
        type: 'code',
        title: 'Flow 2: Archiving a Lost Deal',
        language: 'text',
        code: "1. crm_deal_update     -> stage: \"Lost\"\n2. crm_activity_log    -> kind: \"note\", title: \"Deal lost - competitor pricing\"\n3. crm_record_archive  -> entity_type: \"deal\", id: deal-id\n   (Now hidden from normal queries, but restorable)",
      },
      {
        type: 'code',
        title: 'Automation 1: New Lead -> Welcome Email (CRM trigger)',
        language: 'text',
        code: "[Stimulus: CRM, event = New Lead]\n   -> [Gmail: Send]  to: {{ $json.changes[0].email }}\n                     subject: \"Welcome, {{ $json.changes[0].name }}!\"\n                     body: personalized welcome / next steps\n   -> [CRM: crm_activity_log]  entity_id: {{ $json.changes[0].id }},\n                     entity_type: \"lead\", kind: \"email\", title: \"Welcome email sent\"\n\nLeads created without an email still fire the trigger - add an If node on\n{{ $json.changes[0].email }} to skip the send.",
      },
      {
        type: 'code',
        title: 'Automation 2: Lead Qualified -> Convert to Deal (loop-safe)',
        language: 'text',
        code: "[Stimulus: CRM, event = Any Change]\n   -> [If: {{ $json.changes[0].entity_type }} == \"lead\"\n         AND {{ $json.changes[0].status }} == \"Qualified\"]\n   -> [CRM: crm_record_overview]  entity_type: \"lead\", id: {{ $json.changes[0].id }}\n   -> [If: {{ $json.summary.deal_count }} == 0]\n   -> [CRM: crm_lead_convert_to_deal]  lead_id: {{ $json.changes[0].id }}\n\nThe deal_count == 0 guard makes this loop-safe: the conversion itself touches\nthe lead again (still Qualified), so the trigger re-fires once more - but by\nthen the lead has a deal and the second If stops the chain.",
      },
      {
        type: 'code',
        title: 'Automation 3: Capture a Facebook Chat -> Lead (push, instant)',
        language: 'text',
        code: "[Stimulus: Facebook, event = message]   (Meta pushes to /webhook/facebook - no polling)\n   -> [Agent/AI node]  extract name, email/phone if mentioned, and a one-line\n                       summary from {{ $json.message }} / {{ $json.from_name }}\n   -> [If: contact info found]\n   -> [CRM: crm_lead_create]  name: {{ $json.from_name }}, source: \"Facebook\",\n                       notes: \"{{ $json.message }}\"\n   -> [CRM: crm_activity_log]  entity_type: \"lead\", kind: \"note\",\n                       title: \"Captured from Facebook Messenger\"",
      },
      {
        type: 'code',
        title: 'Automation 4: Website Form -> Lead (generic webhook, instant)',
        language: 'text',
        code: "[Stimulus: Webhook]   URL: POST /webhook/external/<this-workflow-id>\n                      (paste this URL into your site's form handler)\n   -> [If: {{ $json.email }} is present]\n   -> [CRM: crm_lead_create]  name: {{ $json.name }}, email: {{ $json.email }},\n                      phone: {{ $json.phone }}, source: \"Website\"\n\nSend an Idempotency-Key header or an event_id field in the body if your form\nmight retry - the webhook node dedups on it automatically.",
      },
      {
        type: 'code',
        title: 'Automation 5: Inbox -> Lead (Gmail, polled)',
        language: 'text',
        code: "[Stimulus: Gmail, event = new email matching a filter]   (polled, like CRM)\n   -> [Agent/AI node]  parse the sender + body into name / email / intent\n   -> [CRM: crm_lead_create]  name: {{ $json.parsed.name }}, email: {{ $json.from }},\n                      source: \"Email\", notes: \"{{ $json.subject }}\"",
      },
      {
        type: 'note',
        text: 'The funnel pattern: no matter which channel captures the lead (Facebook, website form, email, the dashboard UI, or you telling the agent in chat), every path ends at crm_lead_create. That makes the CRM lead_created trigger workflow (Automation 1/2) the single place all downstream follow-up logic lives, without each capture workflow needing to know about it.',
      },
    ],
  },
  {
    id: 'services-page',
    category: 'Integrations',
    title: 'Services Page (Connectivity and Integrations)',
    summary: 'Central management for credentials, auth providers, MCP servers, SSH, web search, and messaging.',
    tags: ['services', 'integration', 'credentials', 'ssh', 'oauth', 'mcp'],
    blocks: [
      {
        type: 'paragraph',
        text: 'Services is the integration hub. Most external connectivity is configured here before being used in workflows, tools, or automated tasks.',
      },
      {
        type: 'bullet',
        title: 'Service categories',
        items: [
          'Credentials: encrypted key-value secrets for downstream usage.',
          'MCP Servers: connect/disconnect external MCP providers and inspect available tools.',
          'SSH Servers: register remote hosts for secure command execution.',
          'Web Search Accounts: configure API keys, priority, and usage controls.',
          'Messaging Platforms: set tokens and reconnect services (Telegram, Discord, Slack).',
          'Authentication Providers: connect/disconnect OAuth integrations (Google, Microsoft, Facebook, Instagram).',
        ],
      },
      {
        type: 'note',
        text: 'Best practice: complete Services setup before building advanced workflows, otherwise nodes may fail due to missing credentials or disconnected providers.',
      },
    ],
  },
  {
    id: 'files-page',
    category: 'Data Exchange',
    title: 'Files Page (Incoming and Outgoing Assets)',
    summary: 'Upload, download, and clean up files exchanged between users and agents.',
    tags: ['files', 'upload', 'download', 'incoming', 'outgoing'],
    blocks: [
      {
        type: 'paragraph',
        text: 'Files tracks binary assets in both directions: incoming user uploads and outgoing files produced by agents or workflows.',
      },
      {
        type: 'bullet',
        title: 'File operations',
        items: [
          'Upload files directly to /api/upload with session auth.',
          'Download files via signed route parameters using the current master key.',
          'Delete individual records from incoming or outgoing buckets.',
          'Bulk-delete all file records from both directions.',
        ],
      },
      {
        type: 'note',
        text: 'Use outgoing files as artifacts for long-running automation outputs such as reports, exports, or generated media.',
      },
    ],
  },
  {
    id: 'settings-page',
    category: 'Configuration',
    title: 'Settings and Router Pattern Management',
    summary: 'Tune runtime categories and control tool-routing behavior with testable patterns.',
    tags: ['settings', 'router', 'patterns', 'runtime', 'config'],
    blocks: [
      {
        type: 'paragraph',
        text: 'Settings organizes runtime parameters by category and provides a dedicated Router Pattern editor for controlling tool-selection behavior.',
      },
      {
        type: 'bullet',
        title: 'Configuration domains',
        items: [
          'Auth, memory, router, runtime, scheduler, storage, and web search categories.',
          'Secret-aware input handling for key, token, and password fields.',
          'Prompt-aware textareas for instruction-like settings.',
          'Bulk JSON pattern editor and live router simulation test.',
        ],
      },
      {
        type: 'code',
        title: 'Pattern record shape',
        language: 'json',
        code: "[\n  {\n    \"tool_name\": \"web_search\",\n    \"pattern\": \"latest|news|today\",\n    \"description\": \"Use web search for fresh information\",\n    \"enabled\": true\n  }\n]",
      },
    ],
  },
  {
    id: 'api-surface-map',
    category: 'Reference',
    title: 'API Surface Map',
    summary: 'High-level index of important frontend API endpoints grouped by module.',
    tags: ['api', 'endpoints', 'reference', 'backend'],
    blocks: [
      {
        type: 'table',
        title: 'Common endpoints by module',
        columns: ['Module', 'Representative Endpoints', 'Purpose'],
        rows: [
          ['Models', '/models, /models/bulk, /models/{name}/reset', 'Provider lifecycle, status, and bulk state changes'],
          ['Tools', '/tools, /tools/reload, /tools/{name}', 'Tool inventory refresh and enable/disable'],
          ['Memories', '/runs, /runs/{id}, /memory/recent, /memory/search', 'Run history and persistent memory retrieval'],
          ['Tasks', '/jobs, /jobs/{id}/run, /jobs/{id}/pause, /jobs/{id}/resume', 'Cron scheduling operations'],
          ['Workflows', '/workflows, /workflows/{id}/runs, /workflow-runs/{runId}', 'Graph persistence and execution telemetry'],
          ['Services', '/mcp, /ssh_servers, /websearch/accounts, /integrations/status', 'Integration setup and account state'],
          ['Files', '/files/incoming, /files/outgoing, /upload, /download', 'Asset transfer and retrieval'],
          ['Settings', '/settings, /patterns, /patterns/test', 'Runtime configuration and router behavior'],
        ],
      },
      {
        type: 'paragraph',
        text: 'Most UI modules use centralized helper functions for GET, POST, PUT, and DELETE requests through the same /api prefix.',
      },
    ],
  },
  {
    id: 'operations-playbook',
    category: 'Operations',
    title: 'Operations Playbook and Troubleshooting',
    summary: 'Practical guidance for diagnosing common UI and runtime issues.',
    tags: ['ops', 'troubleshooting', 'debugging', 'playbook'],
    blocks: [
      {
        type: 'bullet',
        title: 'If a page looks empty',
        items: [
          'Use each page Refresh action to verify backend connectivity and endpoint health.',
          'Check websocket status chip in the top bar for realtime transport issues.',
          'Confirm AXON_MASTER_KEY is still valid if API calls suddenly fail.',
        ],
      },
      {
        type: 'bullet',
        title: 'If workflow runs appear stuck',
        items: [
          'Verify the workflow is saved before triggering execution.',
          'Use run history panel to inspect status transitions and node results.',
          'Stop run, clear execution state, and re-run with a smaller graph segment.',
          'Validate required service credentials and model/tool availability first.',
        ],
      },
      {
        type: 'bullet',
        title: 'If integrations do not respond',
        items: [
          'Re-open Services and verify provider authentication is still connected.',
          'Test account-specific features (for example search or messaging reconnect).',
          'Inspect tool inventory after integration updates to ensure tool discovery succeeded.',
        ],
      },
      {
        type: 'note',
        text: 'Use this Docs page as a runbook: search by endpoint, module name, or feature keyword to jump directly to the relevant section.',
      },
    ],
  },
]

const searchQuery = ref('')
const activeSectionId = ref(DOC_SECTIONS[0]?.id || '')
let sectionObserver = null

// The docs search lives in the shell topbar, like every other page.
useHeaderSearch('docs', {
  query: searchQuery,
  placeholder: 'Search docs: pages, endpoints, workflows…',
})

function normalizeText(text) {
  return String(text || '')
    .toLowerCase()
    .replace(/\s+/g, ' ')
    .trim()
}

function blockToText(block) {
  if (!block) return ''
  if (block.type === 'paragraph' || block.type === 'note') return block.text || ''
  if (block.type === 'code') return `${block.title || ''} ${block.code || ''}`
  if (block.type === 'bullet') return `${block.title || ''} ${(block.items || []).join(' ')}`
  if (block.type === 'table') {
    const flatRows = (block.rows || []).flat().join(' ')
    return `${block.title || ''} ${(block.columns || []).join(' ')} ${flatRows}`
  }
  return ''
}

const searchableSections = DOC_SECTIONS.map((section) => {
  const indexText = normalizeText(
    [
      section.category,
      section.title,
      section.summary,
      ...(section.tags || []),
      ...(section.blocks || []).map((block) => blockToText(block)),
    ].join(' ')
  )
  return { ...section, indexText }
})

const searchTokens = computed(() => {
  const normalized = normalizeText(searchQuery.value)
  return normalized ? normalized.split(' ').filter(Boolean) : []
})

const filteredSections = computed(() => {
  const tokens = searchTokens.value
  if (!tokens.length) return searchableSections

  return searchableSections
    .map((section) => {
      let score = 0
      for (const token of tokens) {
        const count = section.indexText.split(token).length - 1
        if (count <= 0) return null
        score += count
      }
      return { ...section, score }
    })
    .filter(Boolean)
    .sort((a, b) => (b.score || 0) - (a.score || 0))
})

const visibleSectionsCount = computed(() => filteredSections.value.length)

function clearSearch() {
  searchQuery.value = ''
}

function jumpToSection(sectionId) {
  const el = document.getElementById(`docs-${sectionId}`)
  if (!el) return
  activeSectionId.value = sectionId
  el.scrollIntoView({ behavior: 'smooth', block: 'start' })
}

function destroyObserver() {
  if (!sectionObserver) return
  sectionObserver.disconnect()
  sectionObserver = null
}

function startObserver() {
  destroyObserver()
  if (typeof IntersectionObserver === 'undefined') return

  sectionObserver = new IntersectionObserver(
    (entries) => {
      const visible = entries
        .filter((entry) => entry.isIntersecting)
        .sort((a, b) => b.intersectionRatio - a.intersectionRatio)
      if (visible[0]) {
        activeSectionId.value = visible[0].target.dataset.sectionId || activeSectionId.value
      }
    },
    {
      root: null,
      rootMargin: '-18% 0px -60% 0px',
      threshold: [0.2, 0.45, 0.7],
    }
  )

  filteredSections.value.forEach((section) => {
    const el = document.getElementById(`docs-${section.id}`)
    if (el) sectionObserver.observe(el)
  })
}

watch(
  filteredSections,
  async (sections) => {
    await nextTick()
    startObserver()
    if (!sections.length) {
      activeSectionId.value = ''
      return
    }
    if (!sections.find((section) => section.id === activeSectionId.value)) {
      activeSectionId.value = sections[0].id
    }
  },
  { immediate: true }
)

onBeforeUnmount(() => {
  destroyObserver()
})
</script>

<template>
  <div class="docs-page">
    <section class="docs-layout">
      <aside class="docs-index-col">
        <div class="docs-index-card">
          <h2>Section Index</h2>
          <p>{{ visibleSectionsCount }} of {{ DOC_SECTIONS.length }} sections visible</p>

          <nav class="docs-index-list">
            <button
              v-for="section in filteredSections"
              :key="`index-${section.id}`"
              type="button"
              class="docs-index-link"
              :class="{ active: section.id === activeSectionId }"
              @click="jumpToSection(section.id)"
            >
              <span class="docs-index-title">{{ section.title }}</span>
              <span class="docs-index-summary">{{ section.summary }}</span>
            </button>
          </nav>
        </div>
      </aside>

      <div class="docs-content-col">
        <template v-if="filteredSections.length">
          <article
            v-for="(section, index) in filteredSections"
            :id="`docs-${section.id}`"
            :key="section.id"
            :data-section-id="section.id"
            class="docs-section-card"
            :style="{ '--stagger-delay': `${Math.min(index * 0.05, 0.4)}s` }"
          >
            <header class="docs-section-header">
              <div>
                <span class="docs-section-kicker">{{ section.category }}</span>
                <h2>{{ section.title }}</h2>
                <p>{{ section.summary }}</p>
              </div>
              <div class="docs-tag-row">
                <span
                  v-for="tag in section.tags"
                  :key="`${section.id}-${tag}`"
                  class="docs-tag"
                >
                  {{ tag }}
                </span>
              </div>
            </header>

            <div class="docs-section-body">
              <template
                v-for="(block, blockIndex) in section.blocks"
                :key="`${section.id}-${blockIndex}`"
              >
                <p
                  v-if="block.type === 'paragraph'"
                  class="docs-paragraph"
                >
                  {{ block.text }}
                </p>

                <div
                  v-else-if="block.type === 'bullet'"
                  class="docs-bullet-block"
                >
                  <h3 v-if="block.title">
                    {{ block.title }}
                  </h3>
                  <ul>
                    <li
                      v-for="(item, itemIndex) in block.items"
                      :key="`${section.id}-${blockIndex}-${itemIndex}`"
                    >
                      {{ item }}
                    </li>
                  </ul>
                </div>

                <div
                  v-else-if="block.type === 'table'"
                  class="docs-table-block"
                >
                  <h3 v-if="block.title">
                    {{ block.title }}
                  </h3>
                  <div class="docs-table-wrap">
                    <table>
                      <thead>
                        <tr>
                          <th
                            v-for="(column, columnIndex) in block.columns"
                            :key="`${section.id}-${blockIndex}-head-${columnIndex}`"
                          >
                            {{ column }}
                          </th>
                        </tr>
                      </thead>
                      <tbody>
                        <tr
                          v-for="(row, rowIndex) in block.rows"
                          :key="`${section.id}-${blockIndex}-row-${rowIndex}`"
                        >
                          <td
                            v-for="(cell, cellIndex) in row"
                            :key="`${section.id}-${blockIndex}-row-${rowIndex}-cell-${cellIndex}`"
                          >
                            {{ cell }}
                          </td>
                        </tr>
                      </tbody>
                    </table>
                  </div>
                </div>

                <div
                  v-else-if="block.type === 'code'"
                  class="docs-code-block"
                >
                  <h3 v-if="block.title">
                    {{ block.title }}
                  </h3>
                  <pre><code>{{ block.code }}</code></pre>
                </div>

                <div
                  v-else-if="block.type === 'note'"
                  class="docs-note-block"
                >
                  {{ block.text }}
                </div>
              </template>
            </div>
          </article>
        </template>

        <section
          v-else
          class="docs-empty-state"
        >
          <h2>No matching sections</h2>
          <p>Try broader keywords, shorter terms, or clear search to browse the complete guide.</p>
          <button
            type="button"
            class="btn docs-clear-btn"
            @click="clearSearch"
          >
            Reset Search
          </button>
        </section>
      </div>
    </section>
  </div>
</template>

<style scoped>
.docs-page {
  --docs-ink: #f6f5ef;
  --docs-muted: #9fb0a8;
  --docs-card: rgba(18, 24, 23, 0.86);
  --docs-card-strong: rgba(23, 30, 28, 0.94);
  --docs-border: rgba(207, 219, 210, 0.14);
  --docs-accent: #d8e6be;
  --docs-accent-soft: rgba(216, 230, 190, 0.18);
  --docs-teal: #b8d6ce;
  --docs-warning: #dfc48a;
  display: flex;
  flex-direction: column;
  gap: 10px;
  color: var(--docs-ink);
  font-family: 'Aptos Display', 'Segoe UI Variable Display', 'Segoe UI', sans-serif;
}

.docs-clear-btn {
  /* Uses the `.btn` class — size/colors come from the global --btn-* tokens. */
  cursor: pointer;
}

.docs-clear-btn:hover:not(:disabled) {
  transform: translateY(-1px);
  background: rgba(255, 255, 255, 0.09);
}

.docs-clear-btn:disabled {
  cursor: not-allowed;
  opacity: 0.45;
}

.docs-layout {
  display: grid;
  grid-template-columns: minmax(230px, 280px) minmax(0, 1fr);
  gap: 18px;
}

.docs-index-col {
  min-width: 0;
}

.docs-index-card {
  position: sticky;
  top: 8px;
  border: 1px solid var(--docs-border);
  border-radius: 16px;
  background: var(--docs-card);
  padding: 14px;
}

.docs-index-card h2 {
  font-size: 0.92rem;
  letter-spacing: 0.01em;
}

.docs-index-card p {
  margin-top: 4px;
  margin-bottom: 12px;
  font-size: 0.76rem;
  color: var(--docs-muted);
}

.docs-index-list {
  display: flex;
  flex-direction: column;
  gap: 8px;
}

.docs-index-link {
  width: 100%;
  border: 1px solid transparent;
  border-radius: 12px;
  background: rgba(255, 255, 255, 0.02);
  color: inherit;
  text-align: left;
  padding: 10px;
  cursor: pointer;
  transition: border-color 0.16s ease, background 0.16s ease, transform 0.16s ease;
}

.docs-index-link:hover {
  border-color: rgba(184, 214, 206, 0.26);
  background: rgba(184, 214, 206, 0.1);
  transform: translateX(2px);
}

.docs-index-link.active {
  border-color: rgba(216, 230, 190, 0.42);
  background: rgba(216, 230, 190, 0.14);
}

.docs-index-title {
  display: block;
  font-size: 0.8rem;
  font-weight: 600;
}

.docs-index-summary {
  display: block;
  margin-top: 5px;
  font-size: 0.69rem;
  line-height: 1.45;
  color: var(--docs-muted);
}

.docs-content-col {
  min-width: 0;
  display: flex;
  flex-direction: column;
  gap: 12px;
}

.docs-section-card {
  border: 1px solid var(--docs-border);
  border-radius: 18px;
  background: var(--docs-card-strong);
  overflow: hidden;
  animation: docs-slide-in 0.28s ease both;
  animation-delay: var(--stagger-delay, 0s);
  scroll-margin-top: 10px;
}

.docs-section-header {
  padding: 16px 16px 12px;
  border-bottom: 1px solid rgba(255, 255, 255, 0.05);
  display: flex;
  gap: 12px;
  justify-content: space-between;
  align-items: flex-start;
}

.docs-section-kicker {
  display: inline-flex;
  margin-bottom: 8px;
  min-height: 24px;
  align-items: center;
  border-radius: 999px;
  padding: 0 9px;
  border: 1px solid rgba(255, 255, 255, 0.08);
  background: rgba(255, 255, 255, 0.03);
  font-size: 0.66rem;
  letter-spacing: 0.08em;
  text-transform: uppercase;
  color: var(--docs-muted);
  font-weight: 700;
}

.docs-section-header h2 {
  font-size: 1.16rem;
  letter-spacing: -0.02em;
}

.docs-section-header p {
  margin-top: 7px;
  max-width: 720px;
  line-height: 1.6;
  color: rgba(246, 245, 239, 0.74);
}

.docs-tag-row {
  display: flex;
  flex-wrap: wrap;
  justify-content: flex-end;
  gap: 6px;
  max-width: 320px;
}

.docs-tag {
  min-height: 24px;
  border-radius: 999px;
  padding: 0 9px;
  border: 1px solid rgba(223, 196, 138, 0.24);
  background: rgba(223, 196, 138, 0.12);
  color: var(--docs-warning);
  font-size: 0.66rem;
  display: inline-flex;
  align-items: center;
}

.docs-section-body {
  display: flex;
  flex-direction: column;
  gap: 14px;
  padding: 14px 16px 18px;
}

.docs-paragraph {
  line-height: 1.65;
  color: rgba(246, 245, 239, 0.87);
}

.docs-bullet-block h3,
.docs-table-block h3,
.docs-code-block h3 {
  margin-bottom: 8px;
  font-size: 0.84rem;
  letter-spacing: 0.04em;
  text-transform: uppercase;
  color: var(--docs-muted);
}

.docs-bullet-block ul {
  padding-left: 18px;
  display: grid;
  gap: 8px;
}

.docs-bullet-block li {
  line-height: 1.58;
  color: rgba(246, 245, 239, 0.84);
}

.docs-table-wrap {
  overflow-x: auto;
  border-radius: 12px;
  border: 1px solid rgba(255, 255, 255, 0.08);
}

.docs-table-wrap table {
  width: 100%;
  border-collapse: collapse;
  min-width: 640px;
}

.docs-table-wrap th,
.docs-table-wrap td {
  padding: 9px 10px;
  text-align: left;
  border-bottom: 1px solid rgba(255, 255, 255, 0.05);
  vertical-align: top;
  font-size: 0.78rem;
  line-height: 1.5;
}

.docs-table-wrap th {
  background: rgba(255, 255, 255, 0.03);
  color: var(--docs-muted);
  letter-spacing: 0.04em;
  text-transform: uppercase;
  font-size: 0.68rem;
}

.docs-table-wrap tr:last-child td {
  border-bottom: 0;
}

.docs-code-block pre {
  border-radius: 12px;
  border: 1px solid rgba(184, 214, 206, 0.18);
  background: rgba(10, 14, 13, 0.85);
  padding: 12px;
  overflow-x: auto;
}

.docs-code-block code {
  font-family: 'Cascadia Code', 'Fira Code', monospace;
  font-size: 0.76rem;
  line-height: 1.55;
  color: var(--docs-teal);
  white-space: pre;
}

.docs-note-block {
  border-radius: 12px;
  border-left: 3px solid rgba(216, 230, 190, 0.56);
  background: rgba(216, 230, 190, 0.1);
  padding: 10px 12px;
  line-height: 1.58;
  color: rgba(246, 245, 239, 0.84);
}

.docs-empty-state {
  border: 1px solid var(--docs-border);
  border-radius: 18px;
  background: var(--docs-card);
  padding: 28px;
  text-align: center;
}

.docs-empty-state h2 {
  font-size: 1.1rem;
}

.docs-empty-state p {
  margin: 10px auto 16px;
  max-width: 560px;
  color: var(--docs-muted);
}

@keyframes docs-slide-in {
  from {
    opacity: 0;
    transform: translateY(8px);
  }
  to {
    opacity: 1;
    transform: translateY(0);
  }
}

@media (max-width: 1160px) {
  .docs-layout {
    grid-template-columns: 1fr;
  }

  .docs-index-card {
    position: static;
  }

  .docs-index-list {
    max-height: 280px;
    overflow-y: auto;
    padding-right: 4px;
  }
}

@media (max-width: 860px) {
  .docs-section-header {
    flex-direction: column;
  }

  .docs-tag-row {
    justify-content: flex-start;
    max-width: 100%;
  }
}

@media (max-width: 640px) {
  .docs-section-header,
  .docs-section-body {
    padding-inline: 12px;
  }
}
</style>
