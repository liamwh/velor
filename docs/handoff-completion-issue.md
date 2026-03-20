# Zsh Completion Issue - Handoff

## Problem Summary

The Zsh completion for the `vel` CLI is not working correctly for the `--prompt` argument. When users press `<TAB>` after `vel once --prompt`, they see command names (auto, init, plan, etc.) instead of the actual available prompts (acp-test, implement-plan, once, test-glob-injection).

## Current State

### What Works
- Binary name fixed: The completion script now correctly uses `vel` (not `velor`) as the binary name
- Completion file is generated at `~/.zsh/completion/_vel`
- Syntax is valid (`zsh -n` passes)
- The `vel internal complete-prompts` command correctly returns the prompt names

### What Doesn't Work
- The `--prompt` argument completion shows command names instead of prompt names
- The state-based completion (`->prompt_list`) isn't triggering the proper completion function

## Files Modified

### `/Users/liam/git/velor/apps/velor-cli/src/completion.rs`

**Changes Made:**
1. Added `BINARY_NAME` constant set to `"vel"` (line ~18)
2. Updated all shell completion generation to use `BINARY_NAME` instead of hardcoded `"velor"`
3. Fixed the `print_zsh_completion()` function to use the constant
4. Changed `--prompt` argument spec from `:_vel_prompts` to `:->prompt_list` to use state-based completion
5. Added a second `case $state in` statement to handle the `prompt_list` state

**Current Code Structure:**
```rust
const BINARY_NAME: &str = "vel";

fn print_zsh_completion() {
    let template = r#"...
        '--prompt+[Prompt name from config]:prompt:->prompt_list'
        ...
        case $words[1] in
            ...subcommands...
        esac

        # Handle states from ->state arguments
        case $state in
            prompt_list)
                local -a prompt_names
                prompt_names=("${(@f)$(vel internal complete-prompts 2>/dev/null)}")
                _describe 'prompt' prompt_names
                ;;
        esac
    "#;

    let script = template
        .replace("{BIN_NAME}", BINARY_NAME)
        .replace("{{", "{")
        .replace("}}", "}");
    print!("{}", script);
}
```

## Testing Performed

1. ✅ `vel internal complete-prompts` returns correct prompts:
   ```
   acp-test
   implement-plan
   once
   test-glob-injection
   ```

2. ✅ Completion file syntax is valid

3. ❌ Actual completion shows wrong values (commands instead of prompts)

## Root Cause Analysis

The issue is likely related to how zsh handles the `->state` mechanism:

1. The `-C` flag in `_arguments -C` should allow state to be set, but the second `case $state in` may not be in the right scope
2. The `$state` variable needs to be declared/initialized properly
3. State handling in zsh completion requires specific ordering - the state case must come AFTER the `_arguments` call

## Potential Solutions to Try

### Option 1: Initialize state variable
Add `local state` before the first case statement:
```zsh
_{BIN_NAME}() {
    local state
    local -a commands
    ...
}
```

### Option 2: Use `_values` instead of state-based completion
Change the argument spec and use an inline function:
```zsh
'--prompt+[Prompt name]:name:->promptlist'
```

Then handle with:
```zsh
case $state in
    promptlist)
        _values 'prompt' "${(@f)$(vel internal complete-prompts 2>/dev/null)}"
        ;;
esac
```

### Option 3: Direct completer function
Use a direct completer instead of state:
```zsh
'--prompt+[Prompt name]:name:_vel_prompt_complete'
```

Then define:
```zsh
_vel_prompt_complete() {
    compadd "${(@f)$(vel internal complete-prompts 2>/dev/null)}"
}
```

### Option 4: Rewrite using `_arguments` with `(*)` pattern
Use the _arguments pattern capability directly.

## Debug Commands

To test the completion manually:
```bash
# 1. Regenerate completion
vel completion --shell zsh > ~/.zsh/completion/_vel

# 2. Check syntax
zsh -n ~/.zsh/completion/_vel

# 3. View generated content
grep -A10 "prompt_list" ~/.zsh/completion/_vel

# 4. Test prompts retrieval
vel internal complete-prompts 2>/dev/null

# 5. Start new shell to test
zsh
vel once --prompt <TAB>
```

## User's Shell Setup

The user has the following in `~/.zshrc`:
```bash
# Vel completion
fpath=(~/.zsh/completion $fpath)
autoload -U compinit && compinit
```

## Next Steps

1. **Test the latest fix** - The `local state` variable has been added. Test in a fresh shell:
   ```bash
   zsh
   vel once --prompt <TAB>
   ```

2. **If still broken, try other solutions** - See Options 2-4 above

3. **Test thoroughly** - After each fix, test in a fresh shell session

4. **Update README.md** - Once working, update the shell completion section to reflect the correct approach

5. **Add tests** - Consider adding automated tests for the completion script generation

## Latest Change (2025-03-14)

Added `local state` variable declaration at the start of the `_vel` function. This ensures the `$state` variable is properly initialized before the `case $state in` statement is executed.

**Before:**
```zsh
_vel() {
    local -a commands
    ...
}
```

**After:**
```zsh
_vel() {
    local state
    local -a commands
    ...
}
```

## References

- Zsh completion guide: http://zsh.sourceforge.net/Doc/Release/Completion-System.html
- State-based completion: Use `->state` in `_arguments` and handle with `case $state`
- The completion file is at: `~/.zsh/completion/_vel`
- The CLI binary is installed at: `~/bin/vel`
