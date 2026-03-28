#!/bin/sh
set -eu
REPO_DIR="/Users/liam/git/velor"
export PATH="/Users/liam/bin:/Applications/Codex.app/Contents/Resources:/opt/zerobrew/prefix/bin:/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin"
if [ -f "$REPO_DIR/.env" ]; then
  set -a
  . "$REPO_DIR/.env"
  set +a
fi
exec "$HOME/bin/vel" serve --cwd "$REPO_DIR"
