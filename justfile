# Velor Agent CLI - justfile commands
# Run 'just' to see all available commands

# Build the velor binary in debug mode
build:
    cargo build

# Build the velor binary in release mode
build-release:
    cargo build --release

# Run the velor once command (single-shot prompt)
once:
    cargo build -q && ./target/debug/velor once

# Run the velor auto command (iterative execution)
auto:
    cargo build -q && ./target/debug/velor auto

# Dry-run the once command to see rendered prompt without executing
once-dry:
    cargo build -q && ./target/debug/velor once --dry-run

# Dry-run the auto command to see rendered prompt without executing
auto-dry:
    cargo build -q && ./target/debug/velor auto --dry-run

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
check: format-rust lint-rust

lint-rust:
    cargo clippy --all-targets --all-features --workspace --quiet --no-deps

# Install velor to ~/bin
install:
    @echo "📦 Building velor binary..."
    cargo build --release -q
    @echo "📥 Installing to ~/bin..."
    @mkdir -p ~/bin
    @cp target/release/velor ~/bin/
    @echo "✅ velor installed to ~/bin/velor"

# Show velor version
version:
    ./target/debug/velor --version

# Show velor help
help:
    ./target/debug/velor --help

# Clean build artifacts
clean:
    cargo clean

# Watch mode for development (requires cargo-watch)
watch:
    cargo watch -x 'build --bin velor' -x test -x 'run --bin velor -- once'

# Run velor with custom prompt (usage: just custom --set "key=value")
custom *args:
    cargo build -q && ./target/debug/velor {{ args }}

# Initialise velor in the current repository
init:
    cargo build -q && ./target/debug/velor init

# Open the config file in $EDITOR
edit-config:
    ${EDITOR:-vim} .velor/velor.toml

# Show available prompts from config
show-prompts:
    @grep -E '^\[prompts\.' .velor/velor.toml | sed 's/\[prompts\.//g' | sed 's/\]//g'

# Test notification configuration
test-notification:
    cargo build -q && ./target/debug/velor test-notification

# List all justfile recipes
default:
    @just --list
