//! Template rendering utilities using MiniJinja.
//!
//! This module provides functionality to render prompt templates using
//! the MiniJinja templating engine, with support for variable substitution.

use std::collections::BTreeMap;
use std::fmt::Write;

/// Renders a template string using the provided variables.
///
/// # Errors
///
/// Returns an error if the template cannot be parsed or rendered.
#[tracing::instrument(level = "debug", ret)]
pub fn render_template(
    template: &str,
    vars: &BTreeMap<String, String>,
) -> color_eyre::eyre::Result<String> {
    let mut env = minijinja::Environment::new();
    env.set_undefined_behavior(minijinja::UndefinedBehavior::Strict);
    env.add_template("prompt", template)
        .map_err(|e| color_eyre::eyre::eyre!("failed to parse template: {e}"))?;

    let tmpl = env
        .get_template("prompt")
        .map_err(|e| color_eyre::eyre::eyre!("missing template: {e}"))?;
    let rendered = tmpl
        .render(vars)
        .map_err(|e| enhance_template_error(e, template, vars))?;

    Ok(rendered)
}

/// Enhances a MiniJinja template error with detailed diagnostic information.
///
/// Extracts the undefined variable name, line information, and
/// formats a helpful error message showing available variables and
/// suggestions for fixing the issue.
fn enhance_template_error(
    err: minijinja::Error,
    template: &str,
    vars: &BTreeMap<String, String>,
) -> color_eyre::eyre::Error {
    use minijinja::ErrorKind;

    let mut report = String::from("Template rendering failed\n\n");

    // Extract error details based on error kind
    let (missing_var, line_info, detail) = match err.kind() {
        ErrorKind::UndefinedError => {
            let line = err
                .line()
                .map(|l| l.to_string())
                .unwrap_or_else(|| "?".to_string());
            // Try to extract variable name from template using range information
            let var_name = if let (Some(range), Some(_src)) = (err.range(), err.template_source()) {
                // Extract the variable name from the template source
                let bytes = template.as_bytes();
                if range.end <= bytes.len() {
                    let error_span = &bytes[range.start..range.end];
                    // Try to extract a variable identifier (alphanumeric and underscore)
                    String::from_utf8_lossy(error_span)
                        .chars()
                        .filter(|c| c.is_alphanumeric() || *c == '_')
                        .collect::<String>()
                } else {
                    "<unknown>".to_string()
                }
            } else {
                "<unknown>".to_string()
            };
            (Some(var_name), format!("line {line}"), "undefined variable")
        }
        _ => {
            let line = err
                .line()
                .map(|l| l.to_string())
                .unwrap_or_else(|| "?".to_string());
            (
                None,
                format!("line {line}"),
                err.detail().unwrap_or("rendering error"),
            )
        }
    };

    // Header section
    writeln!(report, "  Error location: {line_info}").expect("failed to write to string");
    writeln!(report, "  Error kind: {detail}").expect("failed to write to string");

    // Missing variable section
    if let Some(var_name) = &missing_var {
        writeln!(report).expect("failed to write to string");
        writeln!(report, "  Missing variable: `{var_name}`").expect("failed to write to string");
    }

    // Available variables section
    writeln!(report).expect("failed to write to string");
    writeln!(report, "  Available variables ({}):", vars.len()).expect("failed to write to string");
    if vars.is_empty() {
        writeln!(report, "    (none)").expect("failed to write to string");
    } else {
        for key in vars.keys() {
            writeln!(report, "    - {key}").expect("failed to write to string");
        }
    }

    // Context section with template snippet
    if let Some(line_num) = err.line() {
        writeln!(report).expect("failed to write to string");
        writeln!(report, "  Template context (line {line_num}):")
            .expect("failed to write to string");
        let template_lines: Vec<&str> = template.lines().collect();
        if let Some(line_content) = template_lines.get(line_num.saturating_sub(1)) {
            writeln!(report, "    {}", line_content.trim()).expect("failed to write to string");
        }
    }

    // Suggestion section
    writeln!(report).expect("failed to write to string");
    writeln!(report, "  Suggestion:").expect("failed to write to string");
    if let Some(var_name) = &missing_var {
        writeln!(
            report,
            "    Add `{var_name}` to the [vars] section in .velor/velor.toml"
        )
        .expect("failed to write to string");
    } else {
        writeln!(
            report,
            "    Check the template at the error location for syntax issues"
        )
        .expect("failed to write to string");
    }

    color_eyre::eyre::eyre!("{report}\n\n  Original error: {err}")
}

/// Merges variables from multiple sources, with later sources taking precedence.
///
/// The precedence order (highest to lowest) is:
/// 1. CLI `--set` overrides
/// 2. Runtime variables
/// 3. File config variables
#[must_use]
#[tracing::instrument(level = "trace", ret)]
pub fn merge_vars(
    base: &BTreeMap<String, String>,
    overrides: &[(String, String)],
    runtime: &[(String, String)],
) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();

    for (k, v) in base {
        out.insert(k.clone(), v.clone());
    }

    for (k, v) in runtime {
        out.insert(k.clone(), v.clone());
    }

    for (k, v) in overrides {
        out.insert(k.clone(), v.clone());
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_merge_vars_precedence() {
        let mut base = BTreeMap::new();
        base.insert("a".to_string(), "base_a".to_string());
        base.insert("b".to_string(), "base_b".to_string());

        let runtime = vec![("a".to_string(), "runtime_a".to_string())];
        let overrides = vec![("a".to_string(), "override_a".to_string())];

        let result = merge_vars(&base, &overrides, &runtime);

        assert_eq!(result.get("a"), Some(&"override_a".to_string()));
        assert_eq!(result.get("b"), Some(&"base_b".to_string()));
    }

    #[test]
    fn test_render_template_basic() {
        let mut vars = BTreeMap::new();
        vars.insert("name".to_string(), "world".to_string());

        let result = render_template("Hello, {{ name }}!", &vars)
            .expect("template rendering should succeed");
        assert_eq!(result, "Hello, world!");
    }

    #[test]
    fn test_render_template_with_missing_var() {
        let vars = BTreeMap::new();
        let result = render_template("Hello, {{ name }}!", &vars);
        assert!(result.is_err());
    }

    #[test]
    fn test_render_template_empty() {
        let vars = BTreeMap::new();
        let result = render_template("", &vars).expect("empty template should render");
        assert_eq!(result, "");
    }

    #[test]
    fn test_render_template_no_vars() {
        let vars = BTreeMap::new();
        let result = render_template("static text", &vars).expect("static template should render");
        assert_eq!(result, "static text");
    }

    #[test]
    fn test_merge_vars_all_empty() {
        let base = BTreeMap::new();
        let runtime = vec![];
        let overrides = vec![];

        let result = merge_vars(&base, &overrides, &runtime);
        assert!(result.is_empty());
    }

    #[test]
    fn test_render_error_message_includes_missing_var_name() {
        let vars = BTreeMap::new();
        let result = render_template("Hello, {{ missing_var }}!", &vars);
        let err = result.expect_err("should fail with undefined variable");

        let err_msg = err.to_string();
        // Error message should contain the missing variable name
        assert!(
            err_msg.contains("missing_var"),
            "error should mention 'missing_var'"
        );
        // Error message should show available variables count
        assert!(
            err_msg.contains("Available variables (0)"),
            "should show 0 available variables"
        );
        // Error message should include helpful suggestion
        assert!(
            err_msg.contains("Add `missing_var`"),
            "should suggest adding the missing variable"
        );
    }

    #[test]
    fn test_render_error_message_lists_available_vars() {
        let mut vars = BTreeMap::new();
        vars.insert("foo".to_string(), "bar".to_string());
        vars.insert("baz".to_string(), "qux".to_string());

        let result = render_template("Use {{ undefined }} here", &vars);
        let err = result.expect_err("should fail with undefined variable");

        let err_msg = err.to_string();
        // Should list the available variables
        assert!(err_msg.contains("- foo"), "should list 'foo' variable");
        assert!(err_msg.contains("- baz"), "should list 'baz' variable");
        assert!(
            err_msg.contains("Available variables (2)"),
            "should show 2 available variables"
        );
    }

    #[test]
    fn test_render_error_message_includes_line_info() {
        let vars = BTreeMap::new();
        let result = render_template("\n\nLine 3: {{ undefined }}", &vars);
        let err = result.expect_err("should fail with undefined variable");

        let err_msg = err.to_string();
        // Should include line information
        assert!(
            err_msg.contains("line 3"),
            "should mention line 3 where error occurred"
        );
    }
}

#[cfg(test)]
mod proptest_tests {
    use super::*;
    use proptest::prelude::*;

    proptest! {
        #[test]
        fn test_merge_vars_override_takes_precedence(
            base in prop::collection::btree_map(".*", ".*", 0..10),
            runtime in prop::collection::btree_map(".*", ".*", 0..10),
            overrides in prop::collection::btree_map(".*", ".*", 0..10)
        ) {
            // Convert BTreeMaps to slices for merge_vars
            let runtime_vec: Vec<(String, String)> = runtime.into_iter().collect();
            let overrides_vec: Vec<(String, String)> = overrides.into_iter().collect();

            let result = merge_vars(&base, &overrides_vec, &runtime_vec);

            // Overrides should always win
            for (k, v) in &overrides_vec {
                prop_assert_eq!(result.get(k), Some(v));
            }

            // Runtime vars should win if no override
            for (k, v) in &runtime_vec {
                if !overrides_vec.iter().any(|(ok, _)| ok == k) {
                    prop_assert_eq!(result.get(k), Some(v));
                }
            }

            // Base vars should be preserved if no override or runtime
            for (k, v) in &base {
                if !overrides_vec.iter().any(|(ok, _)| ok == k)
                    && !runtime_vec.iter().any(|(rk, _)| rk == k) {
                    prop_assert_eq!(result.get(k), Some(v));
                }
            }
        }

        #[test]
        fn test_merge_vars_idempotent_with_empty_slices(
            base in prop::collection::btree_map(".*", ".*", 0..10)
        ) {
            let result1 = merge_vars(&base, &[], &[]);
            let result2 = merge_vars(&result1, &[], &[]);

            prop_assert_eq!(result1, result2);
        }

        #[test]
        fn test_merge_vars_precedence_order(
            base_val in ".*",
            runtime_val in ".*",
            override_val in ".*",
            key in "[a-z]{1,10}"
        ) {
            let mut base = BTreeMap::new();
            base.insert(key.clone(), base_val.clone());

            let runtime = vec![(key.clone(), runtime_val.clone())];
            let overrides = vec![(key.clone(), override_val.clone())];

            let result = merge_vars(&base, &overrides, &runtime);

            // Override should win
            prop_assert_eq!(result.get(&key), Some(&override_val));
        }

        #[test]
        fn test_render_template_preserves_static_text(
            template in "[a-zA-Z0-9 ]{1,100}"
        ) {
            let vars = BTreeMap::new();
            let result = render_template(&template, &vars);
            prop_assert!(result.is_ok());
            let rendered = result.expect("template should render successfully");
            prop_assert_eq!(rendered, template);
        }

        #[test]
        fn test_render_template_single_var_substitution(
            key in "[a-z]{1,10}",
            value in ".*"
        ) {
            let template = format!("{{{{ {key} }}}}");
            let mut vars = BTreeMap::new();
            vars.insert(key.clone(), value.clone());

            let result = render_template(&template, &vars);
            prop_assert!(result.is_ok());
            let rendered = result.expect("var substitution should succeed");
            prop_assert_eq!(rendered, value);
        }

        #[test]
        fn test_render_template_empty_vars_empty_template(
            vars in prop::collection::btree_map("[a-z]{1,5}", ".*", 0..5)
        ) {
            // Empty template should render regardless of vars
            let empty_template = "";
            let result = render_template(empty_template, &vars);
            prop_assert!(result.is_ok());
            let rendered = result.expect("empty template should render");
            prop_assert_eq!(rendered, "");
        }

        #[test]
        fn test_merge_vars_result_size(
            base_size in 0usize..10,
            runtime_size in 0usize..10,
            override_size in 0usize..10
        ) {
            let base: BTreeMap<String, String> = (0..base_size)
                .map(|i| (format!("base_{}", i), format!("base_val_{}", i)))
                .collect();

            let runtime: Vec<(String, String)> = (0..runtime_size)
                .map(|i| (format!("runtime_{}", i), format!("runtime_val_{}", i)))
                .collect();

            let overrides: Vec<(String, String)> = (0..override_size)
                .map(|i| (format!("override_{}", i), format!("override_val_{}", i)))
                .collect();

            let result = merge_vars(&base, &overrides, &runtime);

            // Result size should be at most base + runtime + override
            // (less if there are key collisions)
            let max_size = base_size + runtime_size + override_size;
            prop_assert!(result.len() <= max_size);
        }
    }
}
