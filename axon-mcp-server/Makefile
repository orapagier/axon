.PHONY: build release run dev creds-init creds-edit install strip help

BINARY   := axon-mcp
TARGET   := target/release/$(BINARY)
DEBUG    := target/debug/$(BINARY)
CREDS    := $(HOME)/.config/axon-mcp/credentials.json

help: ## Show this help
	@grep -E '^[a-zA-Z_-]+:.*?## .*$$' $(MAKEFILE_LIST) | awk 'BEGIN {FS=":.*?## "}; {printf "\033[36m%-18s\033[0m %s\n", $$1, $$2}'

build: ## Debug build (fast compile, larger binary)
	cargo build

release: ## Optimised release build (LTO, stripped)
	cargo build --release
	@echo "Binary: $(TARGET)  ($$(du -sh $(TARGET) | cut -f1))"

run: build ## Debug-build and run
	RUST_LOG=info $(DEBUG)

dev: ## Auto-rebuild and run on file changes (requires cargo-watch)
	cargo watch -x 'run'

creds-init: ## Copy credentials template to ~/.config/axon-mcp/
	@mkdir -p $(HOME)/.config/axon-mcp
	@if [ -f $(CREDS) ]; then \
		echo "credentials.json already exists — skipping."; \
	else \
		cp credentials.example.json $(CREDS) && chmod 600 $(CREDS); \
		echo "Created $(CREDS)"; \
		echo "Edit it with: make creds-edit"; \
	fi

creds-edit: ## Open credentials.json in $$EDITOR
	$${EDITOR:-nano} $(CREDS)

install: release ## Install binary to /usr/local/bin
	sudo install -m 755 $(TARGET) /usr/local/bin/$(BINARY)
	@echo "Installed to /usr/local/bin/$(BINARY)"

strip: release ## Strip debug symbols from release binary
	strip $(TARGET)
	@echo "Stripped binary: $$(du -sh $(TARGET) | cut -f1)"

cross-linux-musl: ## Cross-compile to static Linux binary (requires cross)
	cross build --release --target x86_64-unknown-linux-musl
	@echo "Static binary: target/x86_64-unknown-linux-musl/release/$(BINARY)"

cross-arm64: ## Cross-compile for Linux ARM64 (e.g. Raspberry Pi 4, Oracle ARM)
	cross build --release --target aarch64-unknown-linux-musl

clean: ## Remove build artifacts
	cargo clean
