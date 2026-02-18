# Improve Template Error Diagnostics

## Problem

When a template rendering fails due to an undefined variable, the error message is unhelpful:

```
Error: undefined value (in prompt:9)
```

This doesn't tell you:
- Which variable is undefined
- What variables ARE available
- What line/position in the template

## Solution

Enhance the error handling in `src/template.rs` to provide detailed diagnostic information when template rendering fails.

## Implementation

### File: `src/template.rs`

Modify the `render_template` function to:

1. **Parse MiniJinja error details**: Extract variable name, line number, and column from the error
2. **List available variables**: Show all variables that were passed to the template
3. **Provide helpful context**: Include a snippet of the template around the error location

### Key Changes

Around line 26-28 in `render_template`:

```rust
let rendered = tmpl
    .render(vars)
    .map_err(|e| enhance_template_error(e, template, vars))?;
```

Add a new helper function `enhance_template_error` that:

1. Parses the MiniJinja `Error` to extract:
   - The undefined variable name (from `ErrorKind::UndefinedError`)
   - Line and column information
2. Formats a helpful error message showing:
   - The missing variable name prominently
   - All available variables (sorted alphabetically)
   - The template line with error context
3. Uses `color_eyre`'s `eyre!` macro with section headers for clarity

### Error Message Format

The new error should look like:

```
Error: failed to render template

Missing variable: `implementation_plan`

Available variables (5):
  - check_cmd
  - complete_token
  - pin
  - project_name
  - repo_name

Template error location: line 9

Did you mean: Add `implementation_plan` to your [vars] section in .velor/velor.toml
```

## Files to Modify

- `src/template.rs` - Enhance error handling in `render_template`

## Verification

1. Create a test template with an undefined variable
2. Run `velor auto --prompt <test_template>`
3. Verify the error message shows:
   - The exact variable name that's missing
   - List of available variables
   - Helpful suggestion for fixing it
4. Ensure existing tests still pass
