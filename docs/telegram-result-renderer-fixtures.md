# Telegram Result Renderer Fixtures

## Before (legacy log-style terminal message)

```text
vel serve | completed
runner: codex-gpt-5-4
request: tg-131514025-1774695125419

status: turn completed
milestones:
- tool start: rg -n "serve" apps/velor-cli/src/serve.rs
- tool result: rg -n "serve" apps/velor-cli/src/serve.rs success=true (312 matches)
- tool start: apply_patch
- tool result: apply_patch success=true (<no output>)

output:
Updated serve.rs and docs.

result: completed in 18s
```

## After (compact summary)

```text
✅ Completed: Refactor config loader for presentation tiers
repo=velor | branch=main | runner=codex-gpt-5-4 | duration=18s

Summary
- Updated 2 file(s).
- Verification checks run: 2.

Changed
- apps/velor-cli/src/serve.rs
- docs/codex-telegram-server.md

Result
- Added compact/standard/verbose/raw Telegram rendering.
- Preserved full raw logs under .velor/serve-run-logs.

Hints
- Reply `details` for full logs
- Reply `rerun` to retry
- Reply `diff` for changed files summary
```

## After (failure summary)

```text
❌ Failed: Add integration tests for telegram renderer
repo=velor | branch=main | runner=claude-sonnet-4-6 | duration=42s

Summary
- Run ended with: runner exited non-zero (exit status 101).
- 1 file(s) were modified before completion.

Changed before failure
- apps/velor-cli/src/serve.rs

Failure
- runner exited non-zero (exit status 101)
Likely cause
- Runner command returned a non-zero exit.
Next suggested action
- Reply `rerun` with a refined prompt.
- Reply `details` to inspect full raw logs.
```
