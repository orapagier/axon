"""
LoRA fine-tune Qwen2.5-1.5B-Instruct on Axon tool-calling transcripts.
CPU-only: no CUDA, no bitsandbytes/QLoRA, no Unsloth (all require a GPU this
16GB/integrated-Intel-GPU machine doesn't have). Plain transformers + peft +
trl.SFTTrainer instead.

Run the 10-step smoke test first (--smoke-test) to time fp32 vs bf16 on this
exact CPU before committing to a multi-hour full run -- this hardware (no
AVX512-BF16) may not actually accelerate bf16 matmul over fp32.
"""
import argparse
import json
import time
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[2]
DATA_DIR = REPO_ROOT / "finetune" / "data" / "processed"
OUT_DIR = REPO_ROOT / "finetune" / "checkpoints"
LOCAL_MODEL_DIR = REPO_ROOT / "finetune" / "models" / "Qwen2.5-1.5B-Instruct"


def resolve_base_model():
    """HF download of Qwen/Qwen2.5-1.5B-Instruct was throttled to ~100KB/s on
    this network (mirrors didn't help) -- prefer a manually-downloaded local
    copy in finetune/models/ if the weights file is there, else fall back to
    the HF repo id (a normal network download attempt)."""
    if (LOCAL_MODEL_DIR / "model.safetensors").exists():
        return str(LOCAL_MODEL_DIR)
    return "Qwen/Qwen2.5-1.5B-Instruct"


BASE_MODEL = resolve_base_model()


def load_dataset():
    from datasets import Dataset

    rows = []
    for name in ["train_real.jsonl", "train_synthetic.jsonl"]:
        path = DATA_DIR / name
        if not path.exists():
            continue
        with open(path, encoding="utf-8") as f:
            for line in f:
                line = line.strip()
                if line:
                    rows.append(json.loads(line))
    if not rows:
        raise SystemExit(f"No training examples found in {DATA_DIR} -- run 02_extract_dataset.py / 02b_generate_synthetic.py first")
    print(f"Loaded {len(rows)} examples "
          f"({sum(1 for r in rows if r['source']=='real')} real, "
          f"{sum(1 for r in rows if r['source']=='synthetic')} synthetic)")
    # tool_calls[].function.arguments is free-form JSON that differs per tool
    # (gmail_send's args look nothing like shell_tool's) -- pyarrow's schema
    # inference chokes trying to unify that into one struct type across rows,
    # so each row is stored as a single JSON string column instead and parsed
    # back out in render_text().
    return Dataset.from_list([{"row_json": json.dumps(r)} for r in rows])


def render_text(example, tokenizer):
    row = json.loads(example["row_json"])
    return tokenizer.apply_chat_template(
        row["messages"], tools=row.get("tools") or None,
        tokenize=False, add_generation_prompt=False,
    )


def run_smoke_test(dtype_name, torch_dtype, dataset, tokenizer, model_cls):
    import torch
    from transformers import TrainingArguments
    from trl import SFTTrainer, SFTConfig

    model = model_cls.from_pretrained(BASE_MODEL, torch_dtype=torch_dtype)
    from peft import LoraConfig, get_peft_model
    lora_cfg = LoraConfig(
        r=16, lora_alpha=32, lora_dropout=0.05,
        target_modules=["q_proj", "k_proj", "v_proj", "o_proj", "gate_proj", "up_proj", "down_proj"],
        task_type="CAUSAL_LM",
    )
    model = get_peft_model(model, lora_cfg)
    model.gradient_checkpointing_enable()

    cfg = SFTConfig(
        output_dir=str(OUT_DIR / f"_smoke_{dtype_name}"),
        per_device_train_batch_size=1, gradient_accumulation_steps=16,
        max_steps=10, learning_rate=2e-4, logging_steps=1,
        bf16=(dtype_name == "bf16"), report_to="none",
        dataloader_num_workers=0, max_length=1024, packing=False,
        save_strategy="no",
    )
    trainer = SFTTrainer(model=model, args=cfg, train_dataset=dataset,
                         formatting_func=lambda ex: render_text(ex, tokenizer))
    start = time.time()
    trainer.train()
    elapsed = time.time() - start
    print(f"[{dtype_name}] 10 steps took {elapsed:.1f}s ({elapsed/10:.1f}s/step)")
    return elapsed


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--smoke-test", action="store_true", help="time 10 steps in fp32 and bf16, then exit")
    ap.add_argument("--dtype", default="fp32", choices=["fp32", "bf16"], help="dtype for the full run")
    ap.add_argument("--epochs", type=int, default=3)
    ap.add_argument("--lora-r", type=int, default=16)
    args = ap.parse_args()

    import torch
    from transformers import AutoModelForCausalLM, AutoTokenizer
    from peft import LoraConfig, get_peft_model
    from trl import SFTTrainer, SFTConfig

    tokenizer = AutoTokenizer.from_pretrained(BASE_MODEL)
    dataset = load_dataset()

    if args.smoke_test:
        fp32_time = run_smoke_test("fp32", torch.float32, dataset, tokenizer, AutoModelForCausalLM)
        bf16_time = run_smoke_test("bf16", torch.bfloat16, dataset, tokenizer, AutoModelForCausalLM)
        faster = "bf16" if bf16_time < fp32_time else "fp32"
        n = len(dataset)
        steps_per_epoch = max(1, n // 16)
        est_epochs = 5 if n < 200 else (3 if n < 800 else 2)
        est_seconds = (bf16_time if faster == "bf16" else fp32_time) / 10 * steps_per_epoch * est_epochs
        print(f"\nFaster dtype on this CPU: {faster}")
        print(f"Estimated full run ({n} examples, {steps_per_epoch} steps/epoch x {est_epochs} epochs): "
              f"~{est_seconds/60:.0f} min")
        return

    torch_dtype = torch.bfloat16 if args.dtype == "bf16" else torch.float32
    model = AutoModelForCausalLM.from_pretrained(BASE_MODEL, torch_dtype=torch_dtype)

    n = len(dataset)
    lora_r = args.lora_r if n >= 200 else min(args.lora_r, 8)
    lora_cfg = LoraConfig(
        r=lora_r, lora_alpha=lora_r * 2, lora_dropout=0.05,
        target_modules=["q_proj", "k_proj", "v_proj", "o_proj", "gate_proj", "up_proj", "down_proj"],
        task_type="CAUSAL_LM",
    )
    model = get_peft_model(model, lora_cfg)
    model.gradient_checkpointing_enable()
    model.print_trainable_parameters()

    cfg = SFTConfig(
        output_dir=str(OUT_DIR),
        per_device_train_batch_size=1, gradient_accumulation_steps=16,
        num_train_epochs=args.epochs, learning_rate=2e-4,
        lr_scheduler_type="cosine", warmup_ratio=0.03,
        bf16=(args.dtype == "bf16"), optim="adamw_torch",
        save_strategy="epoch", save_total_limit=2,
        logging_steps=5, dataloader_num_workers=0, max_grad_norm=1.0,
        max_length=2048, packing=False, report_to="none",
    )
    trainer = SFTTrainer(model=model, args=cfg, train_dataset=dataset,
                         formatting_func=lambda ex: render_text(ex, tokenizer))
    trainer.train()
    trainer.save_model(str(OUT_DIR / "final"))
    tokenizer.save_pretrained(str(OUT_DIR / "final"))
    print(f"Saved final LoRA adapter to {OUT_DIR / 'final'}")


if __name__ == "__main__":
    main()
