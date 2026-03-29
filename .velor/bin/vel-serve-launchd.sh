#!/bin/sh
set -eu
REPO_DIR="/Users/liam/git/velor"
AGENT_CWD="/Users/liam/git"
export PATH="/Users/liam/bin:/opt/homebrew/bin:/opt/zerobrew/prefix/bin:/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin"
if [ -f "$REPO_DIR/.env" ]; then
  set -a
  . "$REPO_DIR/.env"
  set +a
fi
exec "$HOME/bin/vel" serve --config "$REPO_DIR/.velor/velor.toml" --cwd "$AGENT_CWD"
