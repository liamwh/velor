//! Project management command handlers for velor.
//!
//! This module provides CLI commands for managing the project registry,
//! which is used for multi-repo automation discovery.

use clap::{Args, Subcommand};
use color_eyre::eyre::WrapErr;
use velor_automations::ProjectRegistry;

/// Arguments for the `project` subcommand.
#[derive(Debug, Args)]
pub struct ProjectArgs {
    #[command(subcommand)]
    pub command: ProjectCommand,
}

/// Project management subcommands.
#[derive(Debug, Subcommand)]
pub enum ProjectCommand {
    /// Register a project for automations
    Add {
        /// Path to the project (defaults to current directory)
        path: Option<String>,
        /// Unique identifier for this project
        #[arg(long)]
        id: Option<String>,
    },

    /// Remove a project from the registry
    Remove { id: String },

    /// List all registered projects
    List,

    /// Enable a disabled project
    Enable { id: String },

    /// Disable a project temporarily
    Disable { id: String },
}

/// Runs the `project add` subcommand.
///
/// Registers a new project in the registry for automation discovery.
#[tracing::instrument(level = "debug", ret, err)]
pub async fn run_add(path: Option<String>, id: Option<String>) -> color_eyre::eyre::Result<()> {
    let path = path.unwrap_or_else(|| ".".to_string());
    let path = std::path::PathBuf::from(path);

    let mut registry = ProjectRegistry::load()
        .await
        .wrap_err("Failed to load project registry")?;

    registry
        .add(path, id)
        .await
        .wrap_err("Failed to add project")?;

    registry.save().await.wrap_err("Failed to save registry")?;

    println!("✅ Project registered successfully.");

    Ok(())
}

/// Runs the `project remove` subcommand.
///
/// Removes a project from the registry by ID.
#[tracing::instrument(level = "debug", ret, err, fields(id = %id))]
pub async fn run_remove(id: String) -> color_eyre::eyre::Result<()> {
    let mut registry = ProjectRegistry::load()
        .await
        .wrap_err("Failed to load project registry")?;

    registry
        .remove(&id)
        .await
        .wrap_err_with(|| format!("Failed to remove project '{}'", id))?;

    registry.save().await.wrap_err("Failed to save registry")?;

    println!("✅ Project '{}' removed.", id);

    Ok(())
}

/// Runs the `project list` subcommand.
///
/// Lists all registered projects with their status.
#[tracing::instrument(level = "debug", ret)]
pub async fn run_list() -> color_eyre::eyre::Result<()> {
    let registry = ProjectRegistry::load()
        .await
        .wrap_err("Failed to load project registry")?;

    let projects = registry.list();

    println!("════════════════════════════════════════");
    println!("📁 Registered Projects");
    println!("════════════════════════════════════════");

    if projects.is_empty() {
        println!("\nNo projects registered.");
        println!("Add one with: vel project add <path>");
        return Ok(());
    }

    for project in projects {
        let status = if project.enabled { "✅" } else { "❌" };
        println!("{} {} ({})", status, project.id, project.path.display());
    }

    println!("\nTotal: {} project(s)", projects.len());

    Ok(())
}

/// Runs the `project enable` subcommand.
///
/// Enables a disabled project in the registry.
#[tracing::instrument(level = "debug", ret, err, fields(id = %id))]
pub async fn run_enable(id: String) -> color_eyre::eyre::Result<()> {
    let mut registry = ProjectRegistry::load()
        .await
        .wrap_err("Failed to load project registry")?;

    registry
        .enable(&id)
        .await
        .wrap_err_with(|| format!("Failed to enable project '{}'", id))?;

    registry.save().await.wrap_err("Failed to save registry")?;

    println!("✅ Project '{}' enabled.", id);

    Ok(())
}

/// Runs the `project disable` subcommand.
///
/// Disables a project in the registry temporarily.
#[tracing::instrument(level = "debug", ret, err, fields(id = %id))]
pub async fn run_disable(id: String) -> color_eyre::eyre::Result<()> {
    let mut registry = ProjectRegistry::load()
        .await
        .wrap_err("Failed to load project registry")?;

    registry
        .disable(&id)
        .await
        .wrap_err_with(|| format!("Failed to disable project '{}'", id))?;

    registry.save().await.wrap_err("Failed to save registry")?;

    println!("✅ Project '{}' disabled.", id);

    Ok(())
}

/// Main dispatch function for project commands.
///
/// Routes the command to the appropriate handler.
#[tracing::instrument(level = "debug", ret, err)]
pub async fn run_project(args: ProjectArgs) -> color_eyre::eyre::Result<()> {
    match args.command {
        ProjectCommand::Add { path, id } => run_add(path, id).await,
        ProjectCommand::Remove { id } => run_remove(id).await,
        ProjectCommand::List => run_list().await,
        ProjectCommand::Enable { id } => run_enable(id).await,
        ProjectCommand::Disable { id } => run_disable(id).await,
    }
}

// Note: Comprehensive unit tests exist in `crates/automations/src/registry.rs`
// which cover all ProjectRegistry functionality including add, remove, enable,
// disable, and list operations. The CLI command handlers are thin wrappers
// around those tested functions, so we rely on the registry tests for coverage.
