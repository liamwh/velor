# Velor Agent CLI - justfile commands
# Run 'just' to see all available commands

# Build the velor binary in debug mode
build:
    cargo build

# Build the velor binary in release mode
build-release:
    cargo build --release

# Run the vel once command (single-shot prompt)
once:
    cargo build -q && ./target/debug/vel once

# Run the vel auto command (iterative execution)
auto:
    cargo build -q && ./target/debug/vel auto

# Dry-run the once command to see rendered prompt without executing
once-dry:
    cargo build -q && ./target/debug/vel once --dry-run

# Dry-run the auto command to see rendered prompt without executing
auto-dry:
    cargo build -q && ./target/debug/vel auto --dry-run

# Run tests
test:
    cargo nextest run

# Run tests with output
test-verbose:
    cargo nextest run --nocapture

# Format all Rust code in the workspace
format-rust:
    @cargo fmt --all

# Check code formatting
fmt-check:
    cargo fmt -- --check

# Run clippy lints
lint:
    cargo clippy -- -D warnings

# Run all checks (fmt, clippy, tests)
check: format-rust lint-rust lint-typescript

lint-rust:
    cargo clippy --all-targets --all-features --workspace --quiet --no-deps

lint-typescript:
    cd apps/velor && bunx svelte-kit sync && bunx svelte-check --tsconfig ./tsconfig.json --diagnostic-sources "js,svelte" && bunx eslint .

# Install velor CLI to ~/bin (backs up existing binary with timestamp)
install:
    @echo "📦 Building velor CLI binary..."
    cargo build --release -p velor-cli -q
    @echo "📥 Installing to ~/bin..."
    @mkdir -p ~/bin
    @if [ -f ~/bin/vel ]; then \
        backup=~/bin/vel.backup.$(date +%Y%m%d-%H%M%S); \
        echo "💾 Backing up existing vel to $$backup"; \
        cp ~/bin/vel "$$backup"; \
    fi
    @cp target/release/vel ~/bin/vel
    @echo "✅ vel installed to ~/bin/vel"

# Show vel version
version:
    ./target/debug/vel --version

# Show vel help
help:
    ./target/debug/vel --help

# Clean build artifacts
clean:
    cargo clean

# Watch mode for development (requires cargo-watch)
watch:
    cargo watch -x 'build --bin vel' -x test -x 'run --bin vel -- once'

# Run velor with custom prompt (usage: just custom --set "key=value")
custom *args:
    cargo build -q && ./target/debug/velor {{ args }}

# Initialise vel in the current repository
init:
    cargo build -q && ./target/debug/vel init

# Open the config file in $EDITOR
edit-config:
    ${EDITOR:-vim} .velor/velor.toml

# Show available prompts from config
show-prompts:
    @grep -E '^\[prompts\.' .velor/velor.toml | sed 's/\[prompts\.//g' | sed 's/\]//g'

# Test notification configuration
test-notification:
    cargo build -q && ./target/debug/vel test-notification

# Run the Tauri GUI app in dev mode
tauri-dev:
    cd apps/velor && bun run tauri dev

# Install velor CLI and set up launchd service for automations
install-launchd: install
    @bash scripts/install-launchd.sh

# Uninstall the launchd service
uninstall-launchd:
    #!/usr/bin/env bash
    set -e
    echo "🛑 Stopping and removing velor automations service..."
    PLIST_PATH="$HOME/Library/LaunchAgents/com.velor.automations.plist"

    if [ -f "$PLIST_PATH" ]; then
        if launchctl list | grep -q "com.velor.automations"; then
            echo "🔄 Unloading service..."
            launchctl unload "$PLIST_PATH"
        fi
        echo "🗑️  Removing plist..."
        rm "$PLIST_PATH"
        echo "✅ Velor automations service removed"
    else
        echo "ℹ️  No launchd service found"
    fi

# Check status of velor automations launchd service
launchd-status:
    #!/usr/bin/env bash
    set -e
    echo "📊 Velor automations service status:"

    if launchctl list | grep -q "com.velor.automations"; then
        echo "✅ Service is loaded"
        echo ""
        echo "Recent logs:"
        if [ -f ~/.velor/automations.log ]; then
            tail -10 ~/.velor/automations.log
        else
            echo "No logs found yet"
        fi
    else
        echo "❌ Service is not loaded"
        echo "Run 'just install-launchd' to install it"
    fi

# List all justfile recipes
default:
    @just --list
