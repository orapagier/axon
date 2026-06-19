# Axon Project - Agent Instructions

## graphify - Knowledge Graph Builder

**Trigger:** When the user types `/graphify` or asks to build a knowledge graph from files.

### Setup
- Python package `graphifyy` is installed globally (`pip install graphifyy`)
- Claude Code skill: `~/.claude/skills/graphify/SKILL.md`
- Antigravity workflow: `.agents/workflows/graphify.md`

### Quick Reference
```
/graphify .                        # graph current directory
/graphify ./raw                    # graph specific folder  
/graphify ./raw --mode deep        # thorough extraction
/graphify ./raw --update           # incremental update
/graphify query "question"         # query the graph
/graphify path "A" "B"             # shortest path
/graphify explain "Concept"        # explain a node
/graphify add <url>                # fetch + add to graph
```

### How to Run graphify
1. Ensure graphify is importable: `python -c "import graphify"`
2. Detect files: `python -c "from graphify.detect import detect; from pathlib import Path; import json; print(json.dumps(detect(Path('.'))))"`
3. For code files: use `graphify.extract.extract_code()` (tree-sitter AST, no LLM)
4. For docs/images: use your LLM capabilities to extract entities + relationships
5. Build graph: `graphify.graph.build_graph()` → `cluster()` → `analyze()`
6. Output goes to `graphify-out/` (graph.html, graph.json, obsidian/, GRAPH_REPORT.md)

### Key Principles
- Every edge tagged: `EXTRACTED`, `INFERRED`, or `AMBIGUOUS`
- Cache is SHA256-based — re-runs only process changed files
- Multimodal: code (.py .ts .rs .go .js), docs (.md .txt), papers (.pdf), images (.png .jpg)
- Full protocol in `~/.claude/skills/graphify/SKILL.md`
