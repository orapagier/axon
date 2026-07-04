# CRM Developer & User Guide

## Overview

This CRM is a SQLite-backed, MCP (Model Context Protocol) tool server built in Rust. It exposes **33 tools** organized into five domains: **Leads**, **Deals**, **Organizations**, **Activities**, and **Insights/Workflows**. All data is stored in a single `crm.db` file in the configured data directory.

### Agent vs. Workflow Access

Workflow nodes can always call **every** CRM tool. The chat agent gets the **read** tools (list/get/search/overview/pipeline/dashboard/export/backup) by default; the **write** tools (create/update/delete/convert/archive/restore) are workflow-only unless the operator enables **Settings → CRM → "Allow the chat agent to call CRM write tools"** (`crm.agent_write_tools`). The toggle applies immediately, no restart needed. This mirrors the Facebook/Instagram pattern: mutations flow through deliberate, reviewable workflows; conversation stays read-only by default.

### Data Model

```
Organizations (orgs)
    ├── Leads  (org_id → orgs.id, optional)
    │     └── Deals  (contact_id → leads.id, required)
    │               └── Activities (entity_type = 'deal')
    ├── Activities (entity_type = 'org')
    └── Leads → Activities (entity_type = 'lead')
```

**Key relationships:**
- A **Lead** may optionally belong to an **Organization**.
- A **Deal** must have exactly one **Lead** as its contact (`contact_id`), and optionally an **Organization**.
- **Activities** (notes, calls, emails, meetings, tasks) can be attached to any lead, deal, or org.
- Deleting a lead is blocked if it has active deals. Deleting an org is blocked if it has active leads or deals.

### Soft Delete (Archive) vs. Hard Delete

All records support a `deleted_at` column. The preferred removal path is **archive** (soft delete), which hides records from normal queries while preserving them. Hard delete is permanent and requires `"confirm_permanent": true`. The system will refuse a hard delete without this flag.

---

## Enum Reference

| Field | Allowed Values |
|---|---|
| Lead `status` | `Open`, `Contacted`, `Qualified`, `Lost` |
| Deal `stage` | `Prospecting`, `Qualified`, `Proposal`, `Negotiation`, `Won`, `Lost` |
| Activity `kind` | `note`, `call`, `email`, `meeting`, `task`, `other` |
| Activity `entity_type` | `lead`, `deal`, `org` |

### Field Validation Rules
- **email**: must contain `@`, non-empty local and domain parts, domain must include a `.`
- **currency**: exactly 3 uppercase ASCII letters (e.g., `USD`, `EUR`, `GBP`)
- **timestamps** (`occurred_at`, `expected_close`): ISO 8601 / RFC 3339 format, e.g. `2026-05-01T12:00:00Z`. Any offset is accepted on input (`2026-05-01T22:00:00+10:00`), but values are stored normalized to UTC (`2026-05-01T12:00:00.000Z`) so date comparisons and sorting are always correct.
- **tags**: array of strings; duplicates are silently deduplicated (case-insensitive); empty strings are dropped; at most 50 tags of 100 characters each
- **amount**: must be `>= 0`. Send decimal amounts (`12500.50`); internally stored as integer cents (`amount_minor`), rounded half-to-even. Responses include both `amount` (decimal) and `amount_minor` (cents).
- **probability**: integer `0–100` inclusive
- **field lengths**: names/titles and similar short fields max 500 characters; `email`/`phone` max 200; `notes`/`body` max 64 KB

---

## Pagination

All list tools support `limit` (default 50, max 200) and `offset` (default 0). Responses always include `total`, `limit`, and `offset` so you can paginate:

```json
{ "total": 143, "limit": 50, "offset": 0 }
```

To get page 2: set `"offset": 50`. To get page 3: `"offset": 100`.

---

## 1. Leads

Leads represent individual people or contacts — potential customers at any stage of engagement.

### `crm_lead_create`

Creates a new lead. Only `name` is required.

**Duplicate guard:** if an active lead with the same email (case-insensitive) already exists, the call is rejected with an error carrying the existing lead's id — update that lead instead, or pass `"allow_duplicate": true` to create a second lead deliberately.

**Parameters:**

| Param | Type | Required | Notes |
|---|---|---|---|
| `name` | string | ✅ | Contact's full name |
| `email` | string | ❌ | Must be valid email format |
| `phone` | string | ❌ | Free-form phone string |
| `company` | string | ❌ | Company name (freetext, not linked) |
| `org_id` | string | ❌ | ID of an existing Organization to link |
| `status` | string | ❌ | Default: `Open` |
| `source` | string | ❌ | e.g. `Website`, `Referral`, `Cold Outreach` |
| `tags` | array[string] | ❌ | e.g. `["inbound", "priority"]` |
| `notes` | string | ❌ | Free-form notes |
| `allow_duplicate` | boolean | ❌ | Default `false`: reject if an active lead with the same email exists |

**Returns:** `{ "success": true, "id": "uuid", "name": "..." }`

**Example:**
```json
{
  "name": "Taylor Buyer",
  "email": "taylor@example.com",
  "org_id": "org-uuid-here",
  "status": "Contacted",
  "source": "Website",
  "tags": ["inbound", "enterprise"]
}
```

---

### `crm_lead_list`

Lists leads, optionally filtered by status. Returns most-recently-updated first.

**Parameters:**

| Param | Type | Notes |
|---|---|---|
| `status` | string | One of the 4 statuses, or `All` (default) |
| `limit` | integer | Default 50, max 200 |
| `offset` | integer | Default 0 |

**Returns:** `{ "leads": [...], "total": N, "limit": N, "offset": N }`

---

### `crm_lead_get`

Fetches full details for a single lead by ID.

**Parameters:** `id` (required)

**Returns:** Full lead object including all fields and parsed `tags` array.

---

### `crm_lead_update`

Updates any field(s) of an existing lead. Only the `id` is required; all other fields are optional patches. To clear a nullable field, pass `null`.

**Parameters:** Same fields as create, plus `id` (required). Omitted fields keep their current values.

**Behavior of null vs. omitted:**
- Omitted field → value unchanged
- `"field": null` → clears the field to null
- `"field": ""` → treated as null (empty strings normalize to null)

**Example — advance a lead to Qualified:**
```json
{ "id": "lead-uuid", "status": "Qualified", "notes": "Spoke on phone, confirmed budget." }
```

---

### `crm_lead_delete`

Permanently deletes a lead. **Blocked if any deals reference the lead** — active deals must be removed first; archived deals must be restored and deleted (or archive the lead instead). Requires confirmation.

**Parameters:**

| Param | Type | Notes |
|---|---|---|
| `id` | string | Required |
| `confirm_permanent` | boolean | Must be `true` or the call is rejected |

**Cascade:** All activities attached to this lead are also hard-deleted.

> **Prefer `crm_record_archive` instead.** Archive is reversible; delete is not.

---

### `crm_lead_search`

Full-text search across lead `name`, `email`, `company`, `notes`, and `tags`.

**Parameters:** `query` (required), `limit`, `offset`

**Returns:** `{ "results": [...], "total": N, "query": "..." }`

The search uses `LIKE %query%` with proper escaping for `%`, `_`, and `\` characters.

---

### `crm_lead_convert_to_deal`

Converts a lead into a Deal in one atomic transaction. The lead's status is also updated (default: `Qualified`). This is the recommended path from prospecting to active sales.

**Parameters:**

| Param | Type | Notes |
|---|---|---|
| `lead_id` | string | ✅ Required |
| `title` | string | Optional — defaults to `"Opportunity - {company}"` or `"Opportunity - {name}"` |
| `amount` | number | Default 0.0 |
| `currency` | string | Default `USD` |
| `stage` | string | Default `Prospecting` |
| `probability` | integer | 0–100 |
| `org_id` | string | Defaults to the lead's `org_id` |
| `expected_close` | string | ISO 8601 timestamp |
| `tags` | array[string] | Defaults to the lead's tags |
| `notes` | string | Auto-populated: `"Converted from lead 'Name' (id)"` |
| `lead_status` | string | Status to set on the lead after conversion. Default: `Qualified` |

**Returns:**
```json
{
  "success": true,
  "lead_id": "...",
  "deal_id": "...",
  "deal_title": "...",
  "lead_status": "Qualified"
}
```

---

## 2. Deals

Deals (also called opportunities) represent active sales pursuits with monetary value and a pipeline stage.

### `crm_deal_create`

Creates a new deal. Requires a `title` and a valid `contact_id` (must be an existing, non-archived lead).

**Parameters:**

| Param | Type | Required | Notes |
|---|---|---|---|
| `title` | string | ✅ | Deal name |
| `contact_id` | string | ✅ | ID of the associated Lead |
| `amount` | number | ❌ | Default 0.0; must be ≥ 0 |
| `currency` | string | ❌ | Default `USD`; 3-letter uppercase |
| `stage` | string | ❌ | Default `Prospecting` |
| `probability` | integer | ❌ | 0–100 |
| `org_id` | string | ❌ | Link to an Organization |
| `expected_close` | string | ❌ | ISO 8601 timestamp |
| `tags` | array[string] | ❌ | |
| `notes` | string | ❌ | |

---

### `crm_deal_list`

Lists deals, optionally filtered by stage. Returns pipeline totals **per currency** — amounts in different currencies are never added together.

**Parameters:** `stage` (default `All`), `limit`, `offset`

**Returns:**
```json
{
  "deals": [...],
  "total": 12,
  "total_value": { "USD": 141000.0, "EUR": 4000.0 },
  "limit": 50,
  "offset": 0
}
```

Each deal in `deals` carries both `amount` (decimal, e.g. `12500.5`) and `amount_minor` (integer cents, e.g. `1250050`).

---

### `crm_deal_get`

Fetches full details for a single deal by ID.

**Parameters:** `id` (required)

---

### `crm_deal_update`

Updates any field(s) of an existing deal.

**Special behavior for `probability`:** If you send `"probability": null` explicitly, it clears the probability field. If you omit `probability`, the current value is preserved.

**Example — advance a deal to Won:**
```json
{ "id": "deal-uuid", "stage": "Won", "probability": 100 }
```

---

### `crm_deal_delete`

Permanently deletes a deal. Requires `"confirm_permanent": true`.

**Cascade:** All activities attached to this deal are also hard-deleted.

> **Prefer `crm_record_archive` instead.**

---

### `crm_deal_search`

Searches deals by `title`, `notes`, and `tags`.

**Parameters:** `query` (required), `limit`, `offset`

---

### `crm_pipeline_summary`

Returns an aggregate view of the entire deal pipeline, broken down by stage. All value fields are per-currency maps.

**Parameters:** None

**Returns:**
```json
{
  "pipeline": [
    { "stage": "Prospecting", "count": 3, "total_value": { "USD": 26000.0, "EUR": 4000.0 } },
    { "stage": "Proposal", "count": 2, "total_value": { "USD": 55000.0 } },
    { "stage": "Won", "count": 1, "total_value": { "USD": 12500.0 } }
  ],
  "total_deals": 8,
  "closed_deals": 3,
  "total_value": { "USD": 141000.0, "EUR": 4000.0 },
  "won_value": { "USD": 12500.0 },
  "win_rate_pct": 33.3,
  "won_share_of_all_deals_pct": 12.5
}
```

`win_rate_pct` is calculated as `Won / (Won + Lost)` among closed deals only.

---

## 3. Organizations

Organizations represent companies or accounts. Multiple leads and deals can be associated with a single organization.

### `crm_org_create`

Creates a new organization. Only `name` is required.

**Parameters:**

| Param | Type | Notes |
|---|---|---|
| `name` | string | ✅ Required |
| `website` | string | |
| `industry` | string | Free-form text |
| `size` | string | e.g. `1-10`, `11-50`, `51-200`, `201-1000`, `1000+` |
| `country` | string | |
| `phone` | string | |
| `email` | string | Must be valid email |
| `tags` | array[string] | |
| `notes` | string | |
| `allow_duplicate` | boolean | Default `false`: reject if an active org with the same name exists |

**Duplicate guard:** if another active org with the same name (case-insensitive) exists, the call is rejected with an error carrying the existing org's id. Pass `"allow_duplicate": true` to create it anyway — the response then includes a `"warning"` field naming the existing record.

---

### `crm_org_list`

Lists organizations, optionally filtered by industry (case-insensitive match).

**Parameters:** `industry` (optional), `limit`, `offset`

**Returns:** `{ "organizations": [...], "total": N, ... }`

---

### `crm_org_get`

Fetches full org details by ID.

---

### `crm_org_update`

Updates any field(s) of an existing org. To clear a nullable field, pass `null`.

---

### `crm_org_delete`

Permanently deletes an org. Requires `"confirm_permanent": true`.

**Blocked if** the org has any active leads or deals. You must archive or remove those first.

**Cascade:** All activities attached directly to this org are hard-deleted.

---

### `crm_org_search`

Searches orgs by `name`, `industry`, `country`, `website`, `notes`, and `tags`.

---

## 4. Activities

Activities are the history log of interactions — notes, calls, emails, meetings, and tasks — attached to any lead, deal, or org.

### `crm_activity_log`

Logs a new activity on any CRM record. The referenced entity must exist and not be archived.

**Parameters:**

| Param | Type | Required | Notes |
|---|---|---|---|
| `entity_id` | string | ✅ | ID of the lead, deal, or org |
| `entity_type` | string | ✅ | `lead`, `deal`, or `org` |
| `title` | string | ✅ | Short summary |
| `kind` | string | ❌ | Default `note` |
| `body` | string | ❌ | Full details / transcript |
| `occurred_at` | string | ❌ | ISO 8601; defaults to current time |

**Example:**
```json
{
  "entity_id": "deal-uuid",
  "entity_type": "deal",
  "kind": "meeting",
  "title": "Proposal review call",
  "body": "Client was enthusiastic. Requested revised pricing by Friday.",
  "occurred_at": "2026-04-25T14:00:00Z"
}
```

---

### `crm_activity_list`

Lists activities. Can be filtered by entity, entity type, and/or kind. Results are sorted most-recent first by `occurred_at`, then `created_at`.

**Parameters:**

| Param | Type | Notes |
|---|---|---|
| `entity_id` | string | Filter by a specific record's ID |
| `entity_type` | string | `lead`, `deal`, or `org` |
| `kind` | string | Filter by activity kind |
| `limit` | integer | Default 50 |
| `offset` | integer | Default 0 |

**Note:** `entity_id` and `entity_type` are independent filters. You can filter by `entity_type` without `entity_id` to get all activities on, e.g., all deals.

---

### `crm_activity_get`

Fetches a single activity by ID.

---

### `crm_activity_update`

Updates an existing activity. Can also **reassign** an activity to a different entity — but `entity_id` and `entity_type` must always be provided together when reassigning; providing only one is an error.

**Parameters:** `id` (required) + any subset of `entity_id`, `entity_type`, `kind`, `title`, `body`, `occurred_at`

---

### `crm_activity_delete`

Permanently deletes an activity. Requires `"confirm_permanent": true`.

---

## 5. Insights & Workflow Tools

These tools provide cross-entity views, archive management, and data export.

### `crm_search_all`

Searches across organizations, leads, and deals in a single call.

**Parameters:**

| Param | Type | Notes |
|---|---|---|
| `query` | string | ✅ Required |
| `limit_per_type` | integer | Max results per entity type; default 10, max 50 |

**Returns:**
```json
{
  "query": "Northwind",
  "total_results": 3,
  "counts": { "organizations": 1, "leads": 1, "deals": 1 },
  "organizations": [...],
  "leads": [...],
  "deals": [...]
}
```

Note: Activities are **not** included in `search_all`. Use `crm_activity_list` to search activities.

---

### `crm_record_overview`

Returns a 360-degree view of any lead, deal, or org: the entity itself, its linked records, and recent activities.

**Parameters:**

| Param | Type | Notes |
|---|---|---|
| `entity_type` | string | ✅ `lead`, `deal`, or `org` |
| `id` | string | ✅ |
| `related_limit` | integer | Max related records to return; default 10, max 50 |
| `activity_limit` | integer | Max recent activities; default 20, max 100 |

**For a lead, returns:**
- The lead entity
- Linked organization (if any)
- Linked deals (up to `related_limit`)
- Recent activities
- Summary: `deal_count`, `activity_count`

**For a deal, returns:**
- The deal entity
- Linked lead (contact)
- Linked organization
- Recent activities
- Summary: `activity_count`

**For an org, returns:**
- The org entity
- Linked leads (up to `related_limit`)
- Linked deals (up to `related_limit`)
- Recent activities
- Summary: `lead_count`, `deal_count`, `activity_count`

---

### `crm_dashboard_summary`

An operational dashboard snapshot. Useful for a daily sales overview.

**Parameters:**

| Param | Type | Default | Notes |
|---|---|---|---|
| `stale_days` | integer | 30 | Deals/leads not updated in N days are "stale" |
| `closing_within_days` | integer | 30 | Deals closing within N days appear in the alert list |
| `activity_window_days` | integer | 30 | Count of activities in the last N days |
| `list_limit` | integer | 10 | Max stale/closing deals to list |

**Returns:**
```json
{
  "generated_at": "2026-04-26T...",
  "totals": { "organizations": 5, "leads": 23, "deals": 11, "recent_activities": 8 },
  "lead_status_counts": [
    { "key": "Open", "count": 6 },
    { "key": "Contacted", "count": 9 },
    { "key": "Qualified", "count": 5 },
    { "key": "Lost", "count": 3 }
  ],
  "deal_stage_rollup": [
    { "stage": "Prospecting", "count": 2, "total_value": { "USD": 15000.0 } },
    ...
  ],
  "pipeline": {
    "active_pipeline_value": { "USD": 126000.0, "EUR": 4000.0 },
    "weighted_pipeline_value": { "USD": 75600.0, "EUR": 2400.0 },
    "stale_leads": 4,
    "stale_deals": 2,
    "overdue_deals_count": 1,
    "closing_soon_count": 3
  },
  "closing_soon_deals": [...],
  "stale_deals": [...]
}
```

All pipeline value fields are per-currency maps (`{ "USD": ... }`).

**Stale leads** = Open or Contacted leads not updated in `stale_days`.  
**Stale deals** = Active (not Won/Lost) deals not updated in `stale_days`.  
**Weighted pipeline** = Sum of `amount × probability/100` for active deals, per currency.

---

### `crm_record_archive`

Soft-deletes a record. The record is hidden from all normal queries but can be restored.

**Parameters:** `entity_type` (`org`, `lead`, `deal`, `activity`) and `id` (both required)

**Constraints:**
- Archiving an **org** is blocked if it has active leads or deals.
- Archiving a **lead** is blocked if it has active deals.
- Deals and activities can be archived freely.

**Returns:** `{ "success": true, "entity_type": "...", "id": "...", "archived_at": "..." }`

---

### `crm_record_restore`

Restores a previously archived record. Validates that all referenced records (org, lead, etc.) are still active before restoring.

**Parameters:** `entity_type` and `id` (both required)

**Validation on restore:**
- A **lead** restore checks that its linked org (if any) is still active.
- A **deal** restore checks that its `contact_id` lead and `org_id` org (if any) are still active.
- An **activity** restore checks that its parent entity is still active.

---

### `crm_archived_list`

Lists archived records. Can be filtered to a single entity type or show all types together.

**Parameters:** `entity_type` (optional), `limit`, `offset`

**Returns:**
```json
{
  "archived_records": [
    { "entity_type": "deal", "id": "...", "label": "Enterprise Expansion", "deleted_at": "..." }
  ]
}
```

---

### `crm_export_snapshot`

Exports the entire CRM as a JSON snapshot for backup, migration, or audit.

**Parameters:**

| Param | Type | Default | Notes |
|---|---|---|---|
| `include_archived` | boolean | `true` | Include soft-deleted records |
| `to_file` | boolean | auto | Write to a file instead of returning inline. Defaults to `true` when the dataset exceeds **200 records** (an inline dump of a real dataset would blow the agent's context) |

**Inline mode** (small datasets, or explicit `"to_file": false`):
```json
{
  "exported_at": "2026-04-26T...",
  "include_archived": true,
  "counts": { "organizations": 5, "leads": 23, "deals": 11, "activities": 47 },
  "organizations": [...],
  "leads": [...],
  "deals": [...],
  "activities": [...]
}
```

**File mode** (default over 200 records, or explicit `"to_file": true`) writes a timestamped `crm-export-YYYYMMDD-HHMMSS.json` into the data files directory — it appears in the **Files page** and is fetchable by workflow nodes — and returns only:
```json
{
  "success": true,
  "exported_at": "2026-04-26T...",
  "include_archived": true,
  "counts": { "organizations": 5, "leads": 23, "deals": 11, "activities": 47 },
  "total_records": 86,
  "file": "/path/to/data/files/crm-export-20260426-093000.json",
  "file_name": "crm-export-20260426-093000.json"
}
```

Archived records include a non-null `deleted_at` field in the export.

---

### `crm_backup_db`

Backs up the CRM SQLite database itself to a timestamped `crm-backup-YYYYMMDD-HHMMSS.db` file in the data files directory (Files page). Uses SQLite's `VACUUM INTO` — an online backup that is safe while the CRM is in use and produces a compacted copy.

**Parameters:** none

**Returns:**
```json
{
  "success": true,
  "file": "/path/to/data/files/crm-backup-20260426-093000.db",
  "file_name": "crm-backup-20260426-093000.db",
  "size_bytes": 122880
}
```

**Restore:** stop the agent, replace `crm.db` in the data directory with the backup file (renamed to `crm.db`), start the agent.

**Automation:** pair with the scheduler for a weekly backup — a one-node workflow that calls `crm_backup_db` (or `crm_export_snapshot` for a portable JSON copy) on a cron schedule. Old backups are plain files in the Files page; prune them whenever you like.

---

## Common Workflows

### Workflow 1: New Lead → Deal → Close

```
1. crm_org_create       → Create the company account
2. crm_lead_create      → Add the contact, link to org
3. crm_activity_log     → Log initial call (kind: "call")
4. crm_lead_update      → Advance status to "Contacted" or "Qualified"
5. crm_lead_convert_to_deal  → Create deal, auto-update lead status
6. crm_activity_log     → Log proposal meeting (kind: "meeting")
7. crm_deal_update      → Advance stage → "Proposal" → "Negotiation"
8. crm_deal_update      → stage: "Won", probability: 100
```

### Workflow 2: Daily Sales Review

```
1. crm_dashboard_summary    → Check pipeline health, stale deals, closing soon
2. crm_pipeline_summary     → See per-stage breakdown and win rate
3. crm_record_overview      → Drill into any stale deal for full context
4. crm_activity_log         → Log follow-up actions taken
```

### Workflow 3: Archiving a Lost Deal

```
1. crm_deal_update          → stage: "Lost"
2. crm_activity_log         → kind: "note", title: "Deal lost — competitor pricing"
3. crm_record_archive       → entity_type: "deal", id: deal-id
   (Now hidden from normal queries, but restorable)
```

### Workflow 4: Find Everything Related to a Company

```
1. crm_org_search           → query: "Acme" → get org id
2. crm_record_overview      → entity_type: "org", id: org-id
   (Returns linked leads, deals, and activity history)
```

---

## Legacy JSON Import

On first startup (when the `orgs` table is empty), the CRM automatically imports data from two JSON files in the data directory:

- `crm_orgs.json` — array of org objects
- `crm_activities.json` — array of activity objects

**Org JSON shape:**
```json
{
  "id": "uuid",
  "name": "Legacy Corp",
  "website": "https://example.com",
  "industry": "Consulting",
  "size": "11-50",
  "country": "US",
  "phone": null,
  "email": "ops@example.com",
  "tags": ["legacy"],
  "notes": "Migrated from JSON",
  "created_at": "2026-01-01T00:00:00Z",
  "updated_at": "2026-01-01T00:00:00Z"
}
```

Activities referencing entities that don't exist are skipped with a warning log. The import only runs once — it will not re-import on subsequent startups.

---

## Error Reference

| Error Message | Cause |
|---|---|
| `"param 'X' must be one of: ..."` | Invalid enum value |
| `"param 'X' must be a valid email address"` | Malformed email |
| `"param 'X' must be a 3-letter uppercase currency code"` | Bad currency |
| `"param 'X' must be an ISO 8601 / RFC 3339 timestamp"` | Bad date format |
| `"param 'X' cannot be empty"` | Required field set to null or blank |
| `"contact_id 'X' does not match any lead"` | Referenced lead doesn't exist or is archived |
| `"org_id 'X' does not match any organization"` | Referenced org doesn't exist or is archived |
| `"lead 'X' does not exist"` | Activity target doesn't exist |
| `"Cannot delete lead 'X': N linked deal(s) exist"` | Must remove deals before deleting lead |
| `"Cannot delete lead 'X': N archived deal(s) still reference it"` | Restore + delete the archived deals, or archive the lead instead |
| `"Cannot archive organization 'X': archive or reassign linked leads/deals first"` | Org has active dependents |
| `"Permanent delete requires 'confirm_permanent': true"` | Missing delete confirmation |
| `"params 'entity_type' and 'entity_id' must be provided together"` | Partial activity reassignment |
| `"Cannot restore record because X 'Y' is missing or archived"` | Restore blocked by archived dependency |
| `"param 'X' is too long: N characters (max M)"` | Field exceeds its length cap (names/titles 500, email/phone 200, notes/body 64 KB) |
| `"param 'tags' accepts at most 50 tags"` / `"each tag must be at most 100 characters"` | Tag caps exceeded |
| `"A lead with email 'X' already exists: 'Name' (id: ...)"` | Duplicate-email guard — update the existing lead or pass `allow_duplicate: true` |
| `"An organization named 'X' already exists (id: ...)"` | Duplicate-name guard — use the existing org or pass `allow_duplicate: true` |

---

## Database & Performance Notes

- **Location:** `crm.db` lives in the app-data directory — the platform local-data dir (`~/.local/share/axon-mcp` on Linux, `%LOCALAPPDATA%\axon-mcp` on Windows) by default, or `$AXON_DATA_DIR` when that env var is set (same convention as the `data/files` staging dir; a value ending in `/files` means the app-data base is its parent). Point `AXON_DATA_DIR` at a mounted/backed-up volume on servers. On first start after setting it, existing app-data files (including `crm.db`) are copied over from the legacy location automatically.
- **Backups:** `crm_backup_db` runs an online `VACUUM INTO` copy into the Files page directory — safe under WAL while the CRM is in use. Schedule it weekly via a one-node workflow on the scheduler, or use `crm_export_snapshot` for a portable JSON export. Restore = stop agent, swap the backup in as `crm.db`, start agent.
- The database runs in **WAL mode** with `synchronous = NORMAL`, balancing durability and write performance.
- Connections are pooled up to **8 concurrent connections**.
- All entity tables are indexed on `deleted_at`, `updated_at`, and their foreign keys.
- Tags are stored as a JSON string (`["tag1","tag2"]`) and searched with `LIKE`. For large datasets, tag queries are not index-assisted.
- Timestamps are stored as RFC 3339 strings (not Unix integers). `expected_close` and `occurred_at` are normalized to a fixed UTC format (`2026-05-01T12:00:00.000Z`) on write, so lexicographic sorting and comparisons are correct regardless of the offset supplied.
- Deal amounts are stored as integer cents (`amount_minor`), so sums never accumulate floating-point drift. The tool API keeps speaking decimal `amount`.
- The schema is managed by versioned migrations tracked in `PRAGMA user_version`; existing databases are upgraded in place on startup (REAL amounts backfilled to cents, timestamps normalized to UTC).
