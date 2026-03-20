//! launchd service management for macOS.

use color_eyre::eyre::WrapErr;
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::process::Command;

/// Returns the path to the launchd plist file.
///
/// # Errors
///
/// Returns an error if the home directory cannot be determined.
#[must_use]
pub fn plist_path() -> PathBuf {
    dirs::home_dir()
        .map(|h| h.join("Library/LaunchAgents/com.liamwh.velor.plist"))
        .unwrap_or_else(|| PathBuf::from("com.liamwh.velor.plist"))
}

/// Returns the path to the log directory.
///
/// # Errors
///
/// Returns an error if the home directory cannot be determined.
#[must_use]
pub fn log_directory_path() -> PathBuf {
    dirs::home_dir()
        .map(|h| h.join("Library/Logs/velor"))
        .unwrap_or_else(|| PathBuf::from("."))
}

/// Gets the current user's UID for launchctl commands.
///
/// # Errors
///
/// Returns an error if the UID cannot be determined.
fn get_uid() -> color_eyre::eyre::Result<String> {
    let output = Command::new("id")
        .arg("-u")
        .output()
        .wrap_err("Failed to get UID")?;

    if !output.status.success() {
        return Err(color_eyre::eyre::eyre!("id command failed"));
    }

    Ok(String::from_utf8(output.stdout)
        .wrap_err("UID output is not valid UTF-8")?
        .trim()
        .to_string())
}

/// Loads environment variables from global and project .env files.
///
/// Reads from:
/// 1. `~/.config/velor/.env` (global environment)
/// 2. `~/.config/velor/launchd-env` (legacy format, still supported)
///
/// .env files use KEY=value format, one per line. Lines starting with # are comments.
/// Empty lines are ignored.
///
/// # Returns
///
/// A HashMap of environment variable names to values.
fn load_env_file() -> HashMap<String, String> {
    let mut env_vars = HashMap::new();

    let home = match dirs::home_dir() {
        Some(h) => h,
        None => return env_vars,
    };

    // Load from global .env file
    let global_env = home.join(".config").join("velor").join(".env");
    if global_env.exists()
        && let Ok(content) = fs::read_to_string(&global_env)
    {
        parse_env_file(&content, &mut env_vars);
    }

    // Load from legacy launchd-env file (for backwards compatibility)
    let legacy_env = home.join(".config").join("velor").join("launchd-env");
    if legacy_env.exists()
        && let Ok(content) = fs::read_to_string(&legacy_env)
    {
        parse_env_file(&content, &mut env_vars);
    }

    env_vars
}

/// Parses a .env file content into the provided HashMap.
fn parse_env_file(content: &str, env_vars: &mut HashMap<String, String>) {
    for line in content.lines() {
        let line = line.trim();

        // Skip empty lines and comments
        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        // Parse KEY=VALUE format
        if let Some((key, value)) = line.split_once('=') {
            let key = key.trim().to_string();
            let value = value.trim().to_string();
            if !key.is_empty() {
                env_vars.insert(key, value);
            }
        }
    }
}

/// Generates the EnvironmentVariables dict XML for the launchd plist.
fn generate_env_dict() -> String {
    let mut env_vars = load_env_file();

    // Always include PATH with reasonable defaults
    let default_path = "/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin";
    env_vars
        .entry("PATH".to_string())
        .or_insert_with(|| default_path.to_string());

    // Add ~/bin to PATH if it's not already there
    if let Ok(home) = std::env::var("HOME") {
        let home_bin = format!("{}/bin", home);
        let current_path = env_vars
            .get("PATH")
            .cloned()
            .unwrap_or_else(|| default_path.to_string());
        if !current_path.contains(&home_bin) {
            let new_path = format!("{}:{}", home_bin, current_path);
            env_vars.insert("PATH".to_string(), new_path);
        }
    }

    let mut xml = String::from("<key>EnvironmentVariables</key>\n<dict>\n");
    for (key, value) in &env_vars {
        xml.push_str(&format!(
            "    <key>{}</key>\n    <string>{}</string>\n",
            escape_xml_key(key),
            escape_xml_value(value)
        ));
    }
    xml.push_str("</dict>\n");

    xml
}

/// Escapes special XML characters in plist keys.
fn escape_xml_key(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

/// Escapes special XML characters in plist string values.
fn escape_xml_value(s: &str) -> String {
    escape_xml_key(s) // Same escaping for both
}

/// Installs the launchd service.
///
/// # Errors
///
/// Returns an error if:
/// - The binary path cannot be determined
/// - The log directory cannot be created
/// - The plist cannot be written
/// - launchctl commands fail
#[tracing::instrument(level = "debug", ret, err)]
pub async fn run_install(interval: Option<u64>) -> color_eyre::eyre::Result<()> {
    let plist = plist_path();
    let bin_path = std::env::current_exe().wrap_err("Failed to determine binary path")?;
    let log_dir = log_directory_path();

    tokio::fs::create_dir_all(&log_dir)
        .await
        .wrap_err_with(|| format!("Failed to create log directory: {}", log_dir.display()))?;

    let interval_sec = interval.unwrap_or(60);
    let env_dict = generate_env_dict();
    let plist_content = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>com.liamwh.velor</string>
    <key>ProgramArguments</key>
    <array>
        <string>{}</string>
        <string>automations</string>
        <string>tick</string>
    </array>
    <key>StartInterval</key>
    <integer>{}</integer>
    <key>RunAtLoad</key>
    <true/>
    <key>ThrottleInterval</key>
    <integer>10</integer>
    <key>StandardOutPath</key>
    <string>{}/automations.log</string>
    <key>StandardErrorPath</key>
    <string>{}/automations.error.log</string>
{}
</dict>
</plist>
"#,
        bin_path.display(),
        interval_sec,
        log_dir.display(),
        log_dir.display(),
        env_dict
    );

    // Idempotent: try bootout first (ignore failure)
    let uid = get_uid()?;
    let domain = format!("gui/{}", uid);
    let _ = Command::new("launchctl")
        .args(["bootout", &domain, &plist.to_string_lossy()])
        .output();

    tokio::fs::write(&plist, plist_content)
        .await
        .wrap_err_with(|| format!("Failed to write plist: {}", plist.display()))?;

    // Codesign the binary to ensure it can be executed from PATH on macOS
    // This fixes the "killed" issue that occurs with some release builds
    let _ = Command::new("codesign")
        .args(["--force", "--deep", "-s", "-"])
        .arg(&bin_path)
        .output();

    // Bootstrap
    let output = Command::new("launchctl")
        .args(["bootstrap", &domain, &plist.to_string_lossy()])
        .output()
        .wrap_err("Failed to run launchctl bootstrap")?;

    if !output.status.success() {
        return Err(color_eyre::eyre::eyre!(
            "Failed to bootstrap: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }

    // Enable and kickstart
    let _ = Command::new("launchctl")
        .args(["enable", &format!("{}/com.liamwh.velor", domain)])
        .output();
    let _ = Command::new("launchctl")
        .args(["kickstart", "-k", &format!("{}/com.liamwh.velor", domain)])
        .output();

    println!("Velor automations service installed");
    println!(
        "   Interval: {}s | Logs: {}/automations.log",
        interval_sec,
        log_dir.display()
    );
    println!();
    println!("Next steps:");
    println!("  vel project add <path>     Register a project");
    println!("  vel automations status     Check service status");

    Ok(())
}

/// Uninstalls the launchd service.
///
/// # Errors
///
/// Returns an error if the UID cannot be determined.
#[tracing::instrument(level = "debug", ret, err)]
pub async fn run_uninstall() -> color_eyre::eyre::Result<()> {
    let plist = plist_path();

    if !plist.exists() {
        println!("Service not installed");
        return Ok(());
    }

    let uid = get_uid()?;
    let domain = format!("gui/{}", uid);

    // Bootout using plist path
    let _ = Command::new("launchctl")
        .args(["bootout", &domain, &plist.to_string_lossy()])
        .output();

    tokio::fs::remove_file(&plist)
        .await
        .wrap_err_with(|| format!("Failed to remove plist: {}", plist.display()))?;

    println!("Velor automations service uninstalled");

    Ok(())
}

/// Shows the launchd service status.
///
/// # Errors
///
/// Returns an error if the UID cannot be determined.
#[tracing::instrument(level = "debug", ret, err)]
pub async fn run_status() -> color_eyre::eyre::Result<()> {
    let label = "com.liamwh.velor";
    let plist = plist_path();
    let log_path = log_directory_path().join("automations.log");

    let uid = get_uid()?;
    let domain = format!("gui/{}", uid);

    // Try launchctl print first (more detailed), fall back to list
    let print_output = Command::new("launchctl")
        .args(["print", &format!("{}/{}", domain, label)])
        .output();

    let is_running = print_output
        .as_ref()
        .map(|o| o.status.success())
        .unwrap_or(false);

    if is_running {
        println!("Service is running");
        println!("   Label: {}", label);
        println!("   Plist: {}", plist.display());
        println!("   Logs: {}", log_path.display());

        if log_path.exists()
            && let Ok(metadata) = std::fs::metadata(&log_path)
            && let Ok(modified) = metadata.modified()
            && let Ok(duration) = modified.elapsed()
        {
            println!("   Last log: {}s ago", duration.as_secs());
        }

        if log_path.exists() {
            println!();
            println!("Recent logs:");

            let tail = Command::new("tail")
                .arg("-10")
                .arg(&log_path)
                .output()
                .wrap_err("Failed to read log file")?;

            print!("{}", String::from_utf8_lossy(&tail.stdout));
        }
    } else {
        println!("Service is not running");
        if plist.exists() {
            println!("   Plist exists at: {}", plist.display());
        }
        println!("   Run 'vel automations install' to install");
    }

    Ok(())
}
