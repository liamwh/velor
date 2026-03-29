#!/usr/bin/env sh
set -eu

ACTION="${1:-ensure}"
REPO_DIR="${2:-$(cd "$(dirname "$0")/.." && pwd)}"
LABEL="${VEL_SERVE_LABEL:-com.liamwh.velor.serve}"
DOMAIN="gui/$(id -u)"
PLIST="$HOME/Library/LaunchAgents/$LABEL.plist"
RUNNER_SCRIPT="$REPO_DIR/.velor/bin/vel-serve-launchd.sh"
LOG_DIR="$REPO_DIR/.velor/logs"
AGENT_CWD="${VEL_SERVE_AGENT_CWD:-$HOME/git}"
CODEX_BIN="$(command -v codex || true)"
CODEX_DIR=""

if [ -n "$CODEX_BIN" ]; then
  CODEX_DIR="$(dirname "$CODEX_BIN")"
fi

require_macos() {
  if [ "$(uname -s)" != "Darwin" ]; then
    echo "$ACTION is only supported on macOS (launchd)." >&2
    exit 1
  fi
}

write_runner_script() {
  mkdir -p "$REPO_DIR/.velor/bin" "$LOG_DIR"
  cat >"$RUNNER_SCRIPT" <<EOF
#!/bin/sh
set -eu
REPO_DIR="$REPO_DIR"
AGENT_CWD="$AGENT_CWD"
export PATH="$HOME/bin:$CODEX_DIR:/opt/zerobrew/prefix/bin:/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin"
if [ -f "\$REPO_DIR/.env" ]; then
  set -a
  . "\$REPO_DIR/.env"
  set +a
fi
exec "\$HOME/bin/vel" serve --config "\$REPO_DIR/.velor/velor.toml" --cwd "\$AGENT_CWD"
EOF
  chmod 755 "$RUNNER_SCRIPT"
}

write_plist() {
  mkdir -p "$HOME/Library/LaunchAgents"
  cat >"$PLIST" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>$LABEL</string>
  <key>ProgramArguments</key>
  <array>
    <string>$RUNNER_SCRIPT</string>
  </array>
  <key>WorkingDirectory</key>
  <string>$REPO_DIR</string>
  <key>RunAtLoad</key>
  <true/>
  <key>KeepAlive</key>
  <true/>
  <key>StandardOutPath</key>
  <string>$LOG_DIR/vel-serve.stdout.log</string>
  <key>StandardErrorPath</key>
  <string>$LOG_DIR/vel-serve.stderr.log</string>
</dict>
</plist>
EOF
}

ensure_binary() {
  if [ ! -x "$HOME/bin/vel" ]; then
    echo "Missing executable \$HOME/bin/vel. Run 'just install' first." >&2
    exit 1
  fi
}

ensure_codex() {
  if [ -z "$CODEX_BIN" ]; then
    echo "codex not found on PATH in the current shell; cannot configure launchd service." >&2
    exit 1
  fi
}

ensure_agent_cwd() {
  if [ ! -d "$AGENT_CWD" ]; then
    echo "Configured agent cwd does not exist: $AGENT_CWD" >&2
    echo "Create it or set VEL_SERVE_AGENT_CWD to a valid directory." >&2
    exit 1
  fi
}

ensure_service() {
  ensure_binary
  ensure_codex
  ensure_agent_cwd
  write_runner_script
  write_plist
  launchctl bootout "$DOMAIN" "$PLIST" >/dev/null 2>&1 || true
  launchctl enable "$DOMAIN/$LABEL" >/dev/null 2>&1 || true
  launchctl bootstrap "$DOMAIN" "$PLIST"
  launchctl kickstart -k "$DOMAIN/$LABEL"
  echo "vel serve is ensured running and configured for startup."
  echo "Logs: $LOG_DIR/vel-serve.stdout.log and $LOG_DIR/vel-serve.stderr.log"
}

stop_service() {
  launchctl bootout "$DOMAIN" "$PLIST" >/dev/null 2>&1 || true
  launchctl disable "$DOMAIN/$LABEL" >/dev/null 2>&1 || true
  rm -f "$PLIST"
  echo "vel serve launchd service removed."
}

status_service() {
  if launchctl print "$DOMAIN/$LABEL" >/dev/null 2>&1; then
    echo "vel serve launchd service is loaded."
    echo "Plist: $PLIST"
  else
    echo "vel serve launchd service is not loaded."
    exit 1
  fi
}

require_macos
case "$ACTION" in
  ensure)
    ensure_service
    ;;
  stop)
    stop_service
    ;;
  status)
    status_service
    ;;
  *)
    echo "Usage: $0 {ensure|stop|status} [repo_dir]" >&2
    exit 1
    ;;
esac
