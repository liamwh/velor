//! launchd service management for macOS.

use color_eyre::eyre::WrapErr;
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
    <key>EnvironmentVariables</key>
    <dict>
        <key>PATH</key>
        <string>/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin</string>
    </dict>
</dict>
</plist>
"#,
        bin_path.display(),
        interval_sec,
        log_dir.display(),
        log_dir.display()
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

        if log_path.exists() {
            if let Ok(metadata) = std::fs::metadata(&log_path) {
                if let Ok(modified) = metadata.modified() {
                    if let Ok(duration) = modified.elapsed() {
                        println!("   Last log: {}s ago", duration.as_secs());
                    }
                }
            }
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
