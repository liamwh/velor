//! Build script: embeds the git commit hash + dirty status so `vel --version`
//! identifies exactly which source the binary was built from.

use std::process::Command;

fn main() {
    let hash = Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "unknown".to_string());

    let dirty = Command::new("git")
        .args(["status", "--porcelain"])
        .output()
        .ok()
        .map(|o| !o.stdout.is_empty())
        .unwrap_or(false);

    let branch = Command::new("git")
        .args(["rev-parse", "--abbrev-ref", "HEAD"])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty() && s != "HEAD")
        .unwrap_or_default();

    println!("cargo:rustc-env=VELOR_GIT_HASH={hash}");
    println!(
        "cargo:rustc-env=VELOR_GIT_DIRTY={}",
        if dirty { "dirty" } else { "clean" }
    );
    println!("cargo:rustc-env=VELOR_GIT_BRANCH={branch}");
    println!("cargo:rerun-if-changed=.git/HEAD");
}
