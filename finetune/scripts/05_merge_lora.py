"""Merge a trained LoRA adapter into the base Qwen2.5-1.5B-Instruct weights."""
import argparse
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[2]


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--base", default="Qwen/Qwen2.5-1.5B-Instruct")
    ap.add_argument("--adapter", default=str(REPO_ROOT / "finetune" / "checkpoints" / "final"))
    ap.add_argument("--out", default=str(REPO_ROOT / "finetune" / "merged" / "qwen2.5-1.5b-axon"))
    args = ap.parse_args()

    import torch
    from transformers import AutoModelForCausalLM, AutoTokenizer
    from peft import PeftModel

    print(f"Loading base model {args.base}...")
    base = AutoModelForCausalLM.from_pretrained(args.base, torch_dtype=torch.float32)
    tokenizer = AutoTokenizer.from_pretrained(args.base)

    print(f"Loading LoRA adapter from {args.adapter}...")
    model = PeftModel.from_pretrained(base, args.adapter)

    print("Merging adapter into base weights...")
    merged = model.merge_and_unload()

    Path(args.out).mkdir(parents=True, exist_ok=True)
    merged.save_pretrained(args.out)
    tokenizer.save_pretrained(args.out)
    print(f"Saved merged model to {args.out}")


if __name__ == "__main__":
    main()
