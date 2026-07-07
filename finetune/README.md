# Axon local fine-tune pipeline

Fine-tunes Qwen2.5-1.5B-Instruct on real Axon agent transcripts (native tool-calling
format) into a local Ollama model, deployable as an inert opt-in tier in
`crates/axon-agent/config/models.toml` (`role = "local_finetune_experimental"`).

CPU-only pipeline (no CUDA/GPU) — see the plan this was built from at
`C:\Users\Admin\.claude\plans\frolicking-nibbling-hejlsberg.md`.

## Setup

```powershell
cd finetune
python -m venv .venv
.venv\Scripts\activate
pip install -r requirements.txt
```

## Pipeline order

1. `scripts/01_fetch_remote_db.ps1` — pull the real `axon.db` from the GCP deploy target.
2. `scripts/03_fetch_tool_schemas.py` — dump live tool schemas from a running axon-agent.
3. `scripts/02_extract_dataset.py` — build `data/processed/{train,eval}.jsonl` from the DB.
4. `scripts/04_train_lora.py` — LoRA fine-tune on CPU (fp32, gradient checkpointing).
5. `scripts/05_merge_lora.py` — merge adapter into base weights.
6. Convert to GGUF + quantize via `llama.cpp` (cloned into `finetune/llama.cpp/`, not vendored).
7. `ollama create axon-qwen2.5-1.5b-local -f gguf/Modelfile`
8. `scripts/06_eval_tool_calls.py` — verify tool-call validity before trusting the model anywhere.

Everything under `data/`, `checkpoints/`, `merged/`, `gguf/*.gguf`, and `llama.cpp/` is
gitignored — only scripts and this README are tracked.
