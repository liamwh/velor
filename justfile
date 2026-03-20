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
    @mkdir -p ~/bin/.velor-backups
    @if [ -f ~/bin/vel ]; then \
        backup=~/bin/.velor-backups/vel.$(date +%Y%m%d-%H%M%S); \
        echo "💾 Backing up existing vel to $$backup"; \
        cp ~/bin/vel "$$backup"; \
    fi
    @cp target/release/vel ~/bin/vel
    @echo "🔐 Code signing binary for macOS..."
    @codesign --force --deep -s - ~/bin/vel 2>/dev/null || true
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
install-launchd: install-launchd-service

# Restart launchd service with the latest binary (ensures launchd uses updated binary)
restart-launchd: install
    @echo "🔄 Restarting launchd service with latest binary..."
    @~/bin/vel automations uninstall
    @~/bin/vel automations install
    @echo "✅ Launchd service restarted with latest binary"

# Install launchd service (alias for restart-launchd)
install-launchd-service: restart-launchd

# Uninstall the launchd service
uninstall-launchd:
    @~/bin/vel automations uninstall

# Check status of velor automations launchd service
launchd-status:
    @~/bin/vel automations status

# Verify launchd is using the correct binary (debugging helper)
verify-launchd-binary:
    @echo "🔍 Checking launchd configuration..."
    @echo "Plist binary path:"
    @grep -A 3 "ProgramArguments" ~/Library/LaunchAgents/com.liamwh.velor.plist 2>/dev/null | grep vel || echo "  (launchd not installed)"
    @echo "Current ~/bin/vel version:"
    @~/bin/vel --version 2>/dev/null || echo "  (vel not found in ~/bin)"
    @echo "Binary has --prompt-text support:"
    @~/bin/vel once --help 2>&1 | grep -q "prompt-text" && echo "  ✅ Yes" || echo "  ❌ No"
    @echo "Code signature:"
    @codesign -dv ~/bin/vel 2>&1 | grep -E "Identifier|Format" || echo "  (not signed)"

# List all justfile recipes
default:
    @just --list
