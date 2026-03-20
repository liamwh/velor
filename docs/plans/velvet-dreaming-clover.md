# Plan: Require Consecutive Completion Tokens for Auto-Mode Loop Exit

## Context

Currently, the auto-mode loop exits immediately when the model outputs `<promise>COMPLETE</promise>` in a single iteration. This is problematic because:

1. **Accidental completion**: The model might output the completion token during normal work, causing premature loop termination
2. **No confirmation**: There's no validation that completion was intentional across iterations
3. **User expectation**: Users expect the model to explicitly confirm completion across multiple iterations

**Desired behavior**: The completion token must appear in **two consecutive iterations** for the loop to stop.

## Implementation

### File to Modify
`/Users/liam/git/velor/apps/velor-cli/src/main.rs` - Auto-mode loop (lines 1523-1733)

### Changes Required

1. **Add state variable** to track if previous iteration had completion token:
   ```rust
   let mut previous_iteration_completed = false;
   ```

2. **Update completion detection logic** (around line 1698):
   ```rust
   // Current: single occurrence triggers exit
   if iteration_output.contains(complete_token) {
       println!("✅ PRD complete, exiting.");
       return Ok(...);
   }

   // New: require two consecutive occurrences
   if iteration_output.contains(complete_token) {
       if previous_iteration_completed {
           println!("✅ Completion token seen in consecutive iterations, exiting.");
           return Ok(...);
       } else {
           println!("⏳ Completion token found - one more consecutive iteration needed to stop.");
           previous_iteration_completed = true;
       }
   } else {
       previous_iteration_completed = false;
   }
   ```

3. **Handle MaxIterationsReached case**: Ensure that if the loop reaches max iterations with `previous_iteration_completed = true`, the status message still reflects that completion wasn't fully achieved (needs to be explicit that we need consecutive occurrences).

### Verification

1. **Test single occurrence**: Run agent with completion token in one iteration → should continue to next iteration with message about needing consecutive occurrence
2. **Test consecutive occurrences**: Run agent with completion token in two consecutive iterations → should exit with Completed status
3. **Test interrupted consecutive**: Run agent with completion token in iteration 1, not in iteration 2, then in iteration 3 → should NOT exit after iteration 3
4. **Test no occurrence**: Run agent without completion token → should reach MaxIterationsReached
