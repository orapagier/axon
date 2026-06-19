# axon-qdrant

Qdrant setup optimised for Oracle E2 Micro (1GB RAM), no Docker.  
Includes config, systemd units, weekly trim, weekly backup, and daily health checks.

---

## What's Included

```
axon-qdrant/
├── install.sh                    ← run this first, does everything
├── config/
│   └── qdrant.yaml               ← RAM-optimised Qdrant config
├── systemd/
│   ├── qdrant.service            ← Qdrant daemon (MemoryMax=400M)
│   ├── axon-trim.service/timer   ← weekly memory trim (Sun 03:00)
│   ├── axon-backup.service/timer ← weekly snapshot (Sun 04:00)
│   └── axon-health.service/timer ← daily health check (06:00)
└── scripts/
    ├── create-collections.sh     ← creates agent_memory, documents, entities
    ├── axon-trim.sh              ← 5-pass trim cycle
    ├── axon-backup.sh            ← snapshot + compress + remote copy
    ├── axon-health.sh            ← RAM/disk/Qdrant checks
    ├── axon-restore.sh           ← restore from backup interactively
    └── axon-status.sh            ← live dashboard
```

---

## Quick Start

```bash
# 1. Clone or copy this folder to your Oracle server
scp -r axon-qdrant/ ubuntu@your-oracle-ip:~/

# 2. SSH in and run the installer
ssh ubuntu@your-oracle-ip
cd axon-qdrant
bash install.sh

# 3. Configure backup destination (important!)
nano /usr/local/bin/axon-backup   # edit BACKUP_DEST and BACKUP_CMD

# 4. Verify everything
axon-status
```

---

## Memory Architecture

Three collections are created:

| Collection | Purpose | Trim policy |
|---|---|---|
| `agent_memory` | Episodic memory — conversations, facts, summaries | Tiered: ephemeral 30d, standard 6mo |
| `documents` | Document chunks — emails, files, notes | 12 months if unaccessed |
| `entities` | Long-term entity knowledge — people, companies | Never trimmed |

### Vector payload schema

Every vector you insert should include this payload:

```json
{
  "content":       "text that was embedded",
  "source":        "conversation | email | document | note | manual",
  "tier":          "ephemeral | standard | permanent",
  "created_at":    1710000000,
  "expires_at":    1726000000,
  "importance":    0.7,
  "access_count":  0,
  "last_accessed": 1710000000,
  "agent_id":      "axon",
  "tags":          ["client", "budget"]
}
```

### Tier guide

| Tier | Use for | Auto-expires |
|---|---|---|
| `ephemeral` | Routine conversation turns, lookups | 30 days |
| `standard` | Meeting notes, emails, decisions | 6 months if unaccessed |
| `permanent` | Key facts, preferences, relationships | Never |

---

## Key Settings Explained

### Why `on_disk: true` everywhere?

Qdrant by default loads vector data and HNSW indexes into RAM.  
On a 1GB server this would exhaust memory quickly.  
With `on_disk: true`, only metadata and quantized (compressed) vectors  
stay in RAM — full vectors and indexes live on disk and are memory-mapped.

**Trade-off:** Search latency goes from ~1ms to ~5–20ms.  
For an AI agent doing 10–50 searches/minute, this is imperceptible.

### Why `int8` quantization?

Reduces each vector from ~1.5KB (float32, 384 dims) to ~384 bytes.  
4x smaller, ~1% accuracy loss on semantic similarity tasks.  
The quantized copy stays in RAM for fast approximate lookup;  
the full-precision copy on disk is used for final re-ranking.

### Why `MemoryMax=400M` in systemd?

If Qdrant runs unconstrained on a 1GB server, it can balloon during  
bulk indexing operations and trigger an OOM kill of a random process  
(could be your agent, could be sshd — you lose the server).

The 400MB limit means systemd kills *only Qdrant* if it spikes,  
which then auto-restarts cleanly within 10 seconds.

---

## Trim Policy Details

The weekly trim runs 5 passes in order:

```
Pass 1: Hard expiry      — delete where expires_at < now (all tiers)
Pass 2: Ephemeral        — delete ephemeral vectors older than 30 days
Pass 3: Standard aged    — delete standard: >6 months + unaccessed + low importance
Pass 4: Garbage collect  — delete anything never accessed with importance < 0.15
Pass 5: Stale documents  — delete document chunks >12 months, never accessed
```

After 3 years with a moderate use pattern (~50 new vectors/day):

```
Raw generated:    ~55,000 vectors
After trim:       ~15,000–25,000 active vectors
Qdrant RAM usage: ~60–80 MB
```

---

## Backup Strategy

The backup script creates a `.tar.gz` archive of Qdrant snapshots weekly.  
**Configure a remote destination** — local-only backup doesn't protect  
against Oracle instance reclamation (the main long-term risk).

### Option A: Backblaze B2 (recommended — 10GB free)

```bash
# Install rclone
curl https://rclone.org/install.sh | sudo bash

# Configure B2
rclone config
# → New remote → b2 → enter account ID and app key

# Edit /usr/local/bin/axon-backup:
BACKUP_DEST="b2:your-bucket-name/qdrant-backups/"
BACKUP_CMD="rclone sync $BACKUP_DIR $BACKUP_DEST --b2-hard-delete"
```

### Option B: Second Oracle free instance

```bash
# Generate a dedicated backup SSH key
ssh-keygen -t ed25519 -f ~/.ssh/backup_key -N ""
# Copy public key to second instance
ssh-copy-id -i ~/.ssh/backup_key.pub ubuntu@second-instance-ip

# Edit /usr/local/bin/axon-backup:
BACKUP_DEST="ubuntu@second-instance-ip:~/qdrant-backups/"
BACKUP_CMD="rsync -avz -e 'ssh -i ~/.ssh/backup_key' $BACKUP_DIR/ $BACKUP_DEST"
```

---

## Useful Commands

```bash
axon-status     # live dashboard — RAM, disk, collections, timers
axon-health     # full health check with warnings
axon-trim       # run trim cycle immediately (normally weekly)
axon-backup     # run backup immediately (normally weekly)
axon-restore    # interactive restore from backup

# Qdrant API directly
curl http://localhost:6333/healthz
curl http://localhost:6333/collections
curl http://localhost:6333/collections/agent_memory

# Systemd
sudo systemctl status qdrant
sudo journalctl -u qdrant -f
sudo journalctl -u axon-trim -n 50
sudo systemctl list-timers axon-*

# Logs
tail -f /var/log/qdrant/qdrant.log
tail -f /var/log/qdrant/trim.log
tail -f /var/log/qdrant/backup.log
tail -f /var/log/qdrant/health.log
```

---

## Adding a Vector From Your Rust Agent

```rust
use qdrant_client::prelude::*;
use qdrant_client::qdrant::{PointStruct, UpsertPointsBuilder};
use serde_json::json;

async fn store_memory(
    client: &QdrantClient,
    embedding: Vec<f32>,
    content: &str,
    tier: &str,
    importance: f32,
    source: &str,
) -> anyhow::Result<()> {
    let now = chrono::Utc::now().timestamp();
    let expires_at = match tier {
        "ephemeral" => now + 30 * 86400,
        "standard"  => now + 180 * 86400,
        _           => i64::MAX,           // permanent — never expires
    };

    let payload = json!({
        "content":       content,
        "source":        source,
        "tier":          tier,
        "created_at":    now,
        "expires_at":    expires_at,
        "importance":    importance,
        "access_count":  0,
        "last_accessed": now,
        "agent_id":      "axon",
    });

    let point = PointStruct::new(
        uuid::Uuid::new_v4().to_string(),
        embedding,
        payload.as_object().unwrap().clone().into(),
    );

    client.upsert_points(
        UpsertPointsBuilder::new("agent_memory", vec![point])
    ).await?;

    Ok(())
}
```

---

## Qdrant Version Pinning

The installer pins to a specific version. To upgrade:

```bash
# Read the Qdrant changelog first:
# https://github.com/qdrant/qdrant/releases

# Take a backup before upgrading
axon-backup

# Edit install.sh and change QDRANT_VERSION, then re-run
bash install.sh

# Verify data is intact
axon-status
```

Never upgrade across major versions without reading the migration notes.

---

## Projected Resource Usage Over Time

```
              RAM (Qdrant)   Vectors (active)   Disk (storage)
Month 3:         ~45 MB          ~3,000           ~100 MB
Year 1:          ~55 MB          ~8,000           ~250 MB
Year 2:          ~65 MB         ~15,000           ~400 MB
Year 3:          ~70 MB         ~20,000           ~500 MB
Year 5:          ~75 MB         ~25,000           ~600 MB
Year 10:         ~80 MB         ~28,000           ~700 MB

All values assume moderate use (~50 new vectors/day) with
weekly trim enabled. Growth curve flattens after year 2.
```

The setup is sustainable indefinitely from a resource standpoint.  
The Oracle free tier tenure is the only uncertain variable.
