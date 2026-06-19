---
description: /graphify - Build a knowledge graph from any folder of code, docs, papers, or images using graphify
---

# /graphify Workflow

Turn any folder of files into a navigable knowledge graph with community detection, an honest audit trail, and multiple outputs: interactive HTML, Obsidian vault, GraphRAG-ready JSON, and a plain-language GRAPH_REPORT.md.

## Prerequisites
- Python 3.10+ (installed)
- `graphifyy` pip package (installed globally)

## Usage
```
/graphify                          # run on current directory
/graphify ./raw                    # run on a specific folder
/graphify ./raw --mode deep        # more aggressive INFERRED edge extraction
/graphify ./raw --update           # re-extract only changed files, merge into existing graph
/graphify add https://arxiv.org/abs/...   # fetch a paper, save, update graph
/graphify query "what connects X to Y?"   # BFS traversal query
/graphify path "ModuleA" "ModuleB"        # shortest path between concepts
/graphify explain "ConceptName"           # plain-language explanation
```

## Steps

// turbo-all

### Step 1 - Ensure graphify is installed
```powershell
python -c "import graphify"
```
If this fails, run:
```powershell
pip install graphifyy
```

### Step 2 - Detect files
Run detection on the target path (replace `TARGET_PATH` with the user's path, or `.` if none given):
```powershell
python -c "import json; from graphify.detect import detect; from pathlib import Path; result = detect(Path('TARGET_PATH')); print(json.dumps(result))" > .graphify_detect.json
```

Read `.graphify_detect.json` silently and present a summary:
```
Corpus: X files · ~Y words
  code:     N files (.py .ts .rs ...)
  docs:     N files (.md .txt ...)
  papers:   N files (.pdf ...)
  images:   N files
```

- If `total_files` is 0: stop with "No supported files found."
- If `total_words` > 2,000,000 OR `total_files` > 200: warn and ask which subfolder to use.
- Otherwise: proceed to Step 3.

### Step 3 - Extract entities and relationships
For each file, use the graphify Python library to extract structured entities and relationships.

**Code files** (`.py`, `.ts`, `.js`, `.go`, `.rs`, `.java`, etc.):
```python
from graphify.extract import extract_code
result = extract_code(file_path)
```
This uses tree-sitter AST parsing. No LLM needed.

**Document files** (`.md`, `.txt`, `.pdf`):
Use Claude/your LLM to extract structured entities following the schema:
- Each entity: `{name, type, description}`
- Each relationship: `{source, target, type, confidence: "EXTRACTED|INFERRED|AMBIGUOUS"}`

**Image files** (`.png`, `.jpg`, `.webp`):
Use vision capabilities to extract concepts from screenshots, diagrams, whiteboard photos.

### Step 4 - Build graph, cluster, analyze
```python
from graphify.graph import build_graph, cluster, analyze
G = build_graph(all_entities, all_relationships)
communities = cluster(G)
report = analyze(G, communities)
```

### Step 5 - Generate outputs
The pipeline produces these outputs in `graphify-out/`:
```
graphify-out/
├── graph.html       # interactive visualization (vis.js)
├── obsidian/        # Obsidian vault with linked notes
├── wiki/            # Wikipedia-style articles (if --wiki)
├── GRAPH_REPORT.md  # god nodes, surprising connections, suggested questions
├── graph.json       # persistent graph for future queries
└── cache/           # SHA256 cache for incremental updates
```

### Step 6 - Report results
Print the summary from GRAPH_REPORT.md including:
- God nodes (highest-degree concepts)
- Surprising connections with plain-English explanations
- Suggested questions the graph can answer
- Token reduction benchmark (if corpus > 5000 words)

## For Queries
When the user runs `/graphify query "..."`:
1. Load `graphify-out/graph.json`
2. Use BFS (default) or DFS (`--dfs`) traversal from matching nodes
3. Return connected subgraph with relationship explanations

## For Path Finding
When the user runs `/graphify path "A" "B"`:
1. Load the graph
2. Find shortest path between concept A and concept B
3. Show each hop with relationship type and confidence

## Notes
- The Claude Code SKILL.md is at `~/.claude/skills/graphify/SKILL.md` for full protocol details
- Every edge is tagged `EXTRACTED`, `INFERRED`, or `AMBIGUOUS`
- Cache uses SHA256 - re-runs only process changed files
- Works with code, docs, PDFs, and images (multimodal)
