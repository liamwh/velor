# Progress: .agents/rules Implementation (wiggly-mixing-stearns.md)

## Completed (Phase 1 MVP - Core Rules System Integration)

### Core Module (`src/rules.rs`)
- ✅ Implemented all core types: `Rule`, `RuleFrontmatter`, `RulesSet`, `SelectedRules`, `RulesState`, `RulesCache`
- ✅ Implemented `split_frontmatter()` with robust edge case handling
- ✅ Implemented `parse_rule_file()` for loading .mdc rule files
- ✅ Implemented `discover_rules()` for finding rules in `.agents/rules/`
- ✅ Implemented `select_rules()` for deterministic rule selection
- ✅ Implemented `format_rules_for_injection()` and `inject_rules()` for prompt injection
- ✅ Implemented path normalization utilities: `path_relative_to()`, `normalize_file_path_if_safe()`, `validate_rules_directory()`
- ✅ Fixed glob matching bug: `Rule::new()` now correctly adds all patterns to a single GlobSetBuilder
- ✅ Fixed `split_frontmatter()` to handle whitespace-only content gracefully
- ✅ Fixed proptest assertion for edge cases

### Configuration (`src/config.rs`)
- ✅ Added `RulesConfig` struct with `enabled` and `directory` fields
- ✅ Integrated into `FileConfig` merge logic

### Integration (`src/main.rs`)
- ✅ Integrated rules loading in `run_once()`
- ✅ Integrated rules loading in `run_auto()`
- ✅ Modified `run_auto_loop()` to accept optional `rules_set` parameter
- ✅ Inject selected rules into prompts before sending to agent
- ✅ Added `[rules]` section to default `velor.toml` template

### ACP Integration (`src/acp.rs`)
- ✅ Added `files_read_this_turn` tracking to `VelorClient`
- ✅ Added `take_files_read()` method for retrieving files read (prepared for Phase 3)

### Tests
- ✅ All 207 tests pass
- ✅ Unit tests for frontmatter parsing, glob matching, path normalization, rule formatting
- ✅ Integration tests for rule discovery and caching
- ✅ Property tests using proptest for edge cases

## Completed (Phase 3 - Glob-Based Rule Activation and Mid-Iteration Injection)

### New Helper Functions (`src/rules.rs`)
- ✅ Added `check_new_glob_matches()`: Checks files read against glob patterns to find new rules
- ✅ Added `get_rules_by_names()`: Fetches rule contents by name for formatting
- ✅ Added `build_follow_up_prompt_delta()`: Creates follow-up prompts with delta formatting
- ✅ Added `Rule::name()` getter: Returns the rule name for tracking

### Configuration (`src/config.rs`)
- ✅ Added `max_mid_iteration_injections` field to `RulesConfig` (default: 2)

### Multi-Turn Execution (`src/main.rs`)
- ✅ Added `run_auto_iteration_with_session()`: Core multi-turn loop for rule injection
- ✅ Modified `run_auto_loop()`: Creates persistent `RulesState` across iterations
- ✅ Added `run_auto_iteration_acp()`: Helper for creating/closing ACP sessions
- ✅ Implemented logic to switch between ACP session mode and subprocess mode

### ACP Integration (`src/acp.rs`)
- ✅ Added `Debug` derive to `AcpSession` for tracing instrument usage

### Tests
- ✅ All 207 tests pass after Phase 3 implementation

## Completed (Phase 2 - Intelligent Rule Selection)

### New Helper Functions (`src/rules.rs`)
- ✅ Added `IntelligentSelectionResponse` struct for JSON response parsing
- ✅ Added `build_intelligent_selection_prompt()`: Builds prompt for ACP rule selection
- ✅ Added `parse_intelligent_selection_response()`: Parses JSON response with validation
- ✅ Added `extract_json_from_markdown()`: Fallback parser for markdown-wrapped JSON
- ✅ Added `select_rules_with_intelligent()`: Extended rule selection with intelligent rules

### Configuration (`src/config.rs`)
- ✅ Added `intelligent_selection` field to `RulesConfig` (default: false)
- ✅ Added `intelligent_selection_max_rules` field to `RulesConfig` (default: 5)

### Intelligent Selection Integration (`src/main.rs`)
- ✅ Modified `run_auto_iteration_with_session()`: Accepts and uses intelligent rules
- ✅ Added `select_intelligent_rules_acp()`: Creates short-lived ACP session for rule selection
- ✅ Modified `run_auto_iteration_acp()`: Performs intelligent selection before main iteration
- ✅ Intelligent selection happens in separate session to avoid contaminating main conversation

### Tests
- ✅ All 219 tests pass after Phase 2 implementation
- ✅ Unit tests for intelligent selection prompt building
- ✅ Unit tests for JSON response parsing with validation
- ✅ Unit tests for markdown JSON extraction
- ✅ Unit tests for rule selection with intelligent rules

## Remaining Work (Future Enhancements)

### Phase 4: Full Multi-Turn ACP Session Flow
- [ ] Test end-to-end glob-based rule activation
- [ ] Consider adding more sophisticated delta formatting strategies
- [ ] Add integration test for intelligent selection end-to-end flow

## Notes
- Phase 3 is now complete and committed (commit 3ba88a5)
- Phase 2 is now complete (intelligent rule selection implemented)
- The multi-turn logic only works in ACP mode (subprocess mode uses single-shot execution)
- Intelligent selection uses a separate short-lived ACP session per iteration
- Dead code warnings persist for utility functions not yet used
