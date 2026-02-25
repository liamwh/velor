//! Global application state management for the Tauri GUI.
//!
//! This module provides thread-safe access to shared application state including:
//! - Loaded configuration (merged home + repo)
//! - Git root directory
//! - Active executions with cancel tokens
//! - Automation store
//! - Daemon running flag

use color_eyre::Result;
use color_eyre::eyre::WrapErr;
use std::collections::BTreeMap;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{debug, instrument};

use velor_automations::AutomationStore;
use velor_core::{ExecutionConfig, ExecutionId, ExecutionRecord, FileConfig, PromptDef};

/// Active execution with its cancel token.
#[derive(Debug, Clone)]
pub struct ActiveExecution {
    /// The execution record with all state and events.
    pub record: ExecutionRecord,
    /// Cancel token to signal the execution to stop.
    pub cancel_token: Arc<tokio_util::sync::CancellationToken>,
}

impl ActiveExecution {
    /// Creates a new active execution.
    #[must_use]
    pub fn new(config: ExecutionConfig) -> Self {
        let record = ExecutionRecord::new(config);
        let cancel_token = Arc::new(tokio_util::sync::CancellationToken::new());
        Self {
            record,
            cancel_token,
        }
    }

    /// Returns the execution ID.
    #[must_use]
    pub fn id(&self) -> &ExecutionId {
        &self.record.id
    }

    /// Returns true if the execution has been cancelled.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.cancel_token.is_cancelled()
    }
}

/// Global application state.
///
/// This state is shared across all Tauri command handlers and provides
/// thread-safe access to configuration, executions, and automations.
#[derive(Debug, Clone)]
pub struct AppState {
    /// Loaded home config (from ~/.velor/velor.toml).
    home_config: Arc<RwLock<Option<FileConfig>>>,
    /// Loaded repo config (from {git_root}/.velor/velor.toml).
    repo_config: Arc<RwLock<Option<FileConfig>>>,
    /// Merged effective config.
    merged_config: Arc<RwLock<FileConfig>>,
    /// Git root directory.
    git_root: Arc<RwLock<Option<PathBuf>>>,
    /// Active executions keyed by execution ID.
    active_executions: Arc<RwLock<HashMap<String, ActiveExecution>>>,
    /// Historical executions (completed, failed, or cancelled).
    execution_history: Arc<RwLock<Vec<ExecutionRecord>>>,
    /// Automation store for scheduled tasks.
    automation_store: Arc<RwLock<Option<AutomationStore>>>,
    /// Daemon running flag.
    daemon_running: Arc<RwLock<bool>>,
}

impl Default for AppState {
    fn default() -> Self {
        Self::new()
    }
}

impl AppState {
    /// Creates a new application state with default values.
    #[must_use]
    pub fn new() -> Self {
        Self {
            home_config: Arc::new(RwLock::new(None)),
            repo_config: Arc::new(RwLock::new(None)),
            merged_config: Arc::new(RwLock::new(FileConfig::default())),
            git_root: Arc::new(RwLock::new(None)),
            active_executions: Arc::new(RwLock::new(HashMap::new())),
            execution_history: Arc::new(RwLock::new(Vec::new())),
            automation_store: Arc::new(RwLock::new(None)),
            daemon_running: Arc::new(RwLock::new(false)),
        }
    }

    /// Loads configuration from home and repo paths.
    ///
    /// # Errors
    ///
    /// Returns an error if loading or parsing configuration fails.
    #[instrument(skip(self), level = "debug")]
    pub async fn load_configs(
        &self,
        home_path: Option<PathBuf>,
        repo_path: Option<PathBuf>,
    ) -> Result<()> {
        debug!(?home_path, ?repo_path, "Loading configurations");

        // Load home config
        let home_cfg = if let Some(path) = home_path {
            FileConfig::load_if_exists(&path)?
        } else {
            FileConfig::load_if_exists(&FileConfig::home_config_path()?)?
        };

        // Load repo config
        let repo_cfg = if let Some(path) = repo_path {
            FileConfig::load_if_exists(&path)?
        } else {
            None
        };

        // Merge configs (clone references to avoid move)
        let merged = match (&home_cfg, &repo_cfg) {
            (Some(home), Some(repo)) => FileConfig::merge(home.clone(), repo.clone()),
            (Some(home), None) => home.clone(),
            (None, Some(repo)) => repo.clone(),
            (None, None) => FileConfig::default(),
        };

        // Store all configs
        *self.home_config.write().await = home_cfg;
        *self.repo_config.write().await = repo_cfg;
        *self.merged_config.write().await = merged;

        debug!("Configuration loaded successfully");
        Ok(())
    }

    /// Returns the home configuration.
    #[instrument(skip(self), level = "trace", ret)]
    pub async fn home_config(&self) -> Option<FileConfig> {
        self.home_config.read().await.clone()
    }

    /// Returns the repo configuration.
    #[instrument(skip(self), level = "trace", ret)]
    pub async fn repo_config(&self) -> Option<FileConfig> {
        self.repo_config.read().await.clone()
    }

    /// Returns the merged (effective) configuration.
    #[instrument(skip(self), level = "trace", ret)]
    pub async fn merged_config(&self) -> FileConfig {
        self.merged_config.read().await.clone()
    }

    /// Sets the git root directory.
    #[instrument(skip(self), level = "debug")]
    pub async fn set_git_root(&self, path: PathBuf) {
        debug!(?path, "Setting git root");
        *self.git_root.write().await = Some(path);
    }

    /// Returns the git root directory.
    #[instrument(skip(self), level = "trace", ret)]
    pub async fn git_root(&self) -> Option<PathBuf> {
        self.git_root.read().await.clone()
    }

    /// Starts a new execution and adds it to active executions.
    ///
    /// # Errors
    ///
    /// Returns an error if the execution cannot be created.
    #[instrument(skip(self, config), level = "debug", ret)]
    pub async fn start_execution(&self, config: ExecutionConfig) -> Result<ExecutionId> {
        let execution = ActiveExecution::new(config);
        let id = execution.id().as_str().to_string();

        debug!(id, "Starting execution");

        self.active_executions
            .write()
            .await
            .insert(id.clone(), execution);

        Ok(ExecutionId::from_string(id))
    }

    /// Cancels an execution by ID.
    ///
    /// Returns true if the execution was found and cancelled.
    #[instrument(skip(self), level = "debug", ret)]
    pub async fn cancel_execution(&self, id: &ExecutionId) -> Result<bool> {
        let executions = self.active_executions.read().await;
        let id_str = id.as_str();

        if let Some(execution) = executions.get(id_str) {
            debug!(id = id_str, "Cancelling execution");
            execution.cancel_token.cancel();
            Ok(true)
        } else {
            debug!(id = id_str, "Execution not found");
            Ok(false)
        }
    }

    /// Returns an active execution by ID.
    #[instrument(skip(self), level = "trace", ret)]
    pub async fn get_execution(&self, id: &ExecutionId) -> Option<ActiveExecution> {
        let executions = self.active_executions.read().await;
        executions.get(id.as_str()).cloned()
    }

    /// Returns all active executions.
    #[instrument(skip(self), level = "trace", ret)]
    pub async fn active_executions(&self) -> Vec<ActiveExecution> {
        self.active_executions
            .read()
            .await
            .values()
            .cloned()
            .collect()
    }

    /// Moves an execution from active to history.
    ///
    /// Returns true if the execution was found and moved.
    #[instrument(skip(self), level = "debug", ret)]
    pub async fn finish_execution(&self, id: &ExecutionId) -> Result<bool> {
        let id_str = id.as_str();

        // Remove from active
        let execution = self.active_executions.write().await.remove(id_str);

        if let Some(execution) = execution {
            debug!(id = id_str, "Moving execution to history");

            // Add to history
            self.execution_history
                .write()
                .await
                .push(execution.record.clone());

            Ok(true)
        } else {
            debug!(id = id_str, "Active execution not found");
            Ok(false)
        }
    }

    /// Returns the execution history (most recent first).
    #[instrument(skip(self), level = "trace", ret)]
    pub async fn execution_history(&self) -> Vec<ExecutionRecord> {
        let mut history = self.execution_history.read().await.clone();
        history.reverse();
        history
    }

    /// Initializes the automation store with the given database path.
    ///
    /// # Errors
    ///
    /// Returns an error if the store cannot be opened.
    #[instrument(skip(self), level = "debug")]
    pub async fn init_automation_store(&self, db_path: PathBuf) -> Result<()> {
        debug!(?db_path, "Initializing automation store");

        let store = AutomationStore::open(&db_path).await?;
        *self.automation_store.write().await = Some(store);

        debug!("Automation store initialized");
        Ok(())
    }

    /// Returns the automation store if initialized.
    pub async fn automation_store(&self) -> Option<AutomationStore> {
        self.automation_store.read().await.clone()
    }

    /// Sets the daemon running state.
    #[instrument(skip(self), level = "debug")]
    pub async fn set_daemon_running(&self, running: bool) {
        debug!(running, "Setting daemon running state");
        *self.daemon_running.write().await = running;
    }

    /// Returns true if the daemon is running.
    #[instrument(skip(self), level = "trace", ret)]
    pub async fn is_daemon_running(&self) -> bool {
        *self.daemon_running.read().await
    }

    /// Returns all available prompts from the merged config.
    pub async fn available_prompts(&self) -> HashMap<String, String> {
        self.merged_config
            .read()
            .await
            .prompts
            .iter()
            .map(|(k, v)| (k.clone(), v.template().to_string()))
            .collect()
    }

    /// Returns a specific prompt template by name.
    pub async fn get_prompt(&self, name: &str) -> Option<PromptDef> {
        self.merged_config.read().await.prompts.get(name).cloned()
    }

    /// Returns all variables from the merged config.
    pub async fn all_vars(&self) -> BTreeMap<String, String> {
        self.merged_config.read().await.vars.clone()
    }

    /// Returns the effective defaults from the merged config.
    pub async fn effective_defaults(&self) -> velor_core::Defaults {
        self.merged_config.read().await.defaults.clone()
    }

    /// Saves configuration to the specified path.
    ///
    /// # Errors
    ///
    /// Returns an error if the path is invalid or writing fails.
    #[instrument(skip(self, config), level = "debug")]
    pub async fn save_config(&self, config: &FileConfig, path: &PathBuf) -> Result<()> {
        debug!(?path, "Saving configuration");

        // Ensure parent directory exists
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .wrap_err_with(|| format!("Failed to create config directory: {:?}", parent))?;
        }

        // Serialize to TOML
        let toml_str =
            toml::to_string_pretty(config).wrap_err("Failed to serialize configuration to TOML")?;

        // Write to file
        tokio::fs::write(path, toml_str.as_bytes())
            .await
            .wrap_err_with(|| format!("Failed to write config to {:?}", path))?;

        debug!("Configuration saved successfully");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_app_state_default() {
        let state = AppState::new();

        // Check default values
        assert!(state.home_config.try_read().is_ok());
        assert!(state.repo_config.try_read().is_ok());
        assert!(state.merged_config.try_read().is_ok());
        assert!(state.git_root.try_read().is_ok());
        assert!(state.active_executions.try_read().is_ok());
        assert!(state.execution_history.try_read().is_ok());
        assert!(state.automation_store.try_read().is_ok());
        assert!(state.daemon_running.try_read().is_ok());

        // Verify defaults
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            assert!(state.home_config().await.is_none());
            assert!(state.repo_config().await.is_none());
            assert!(state.git_root().await.is_none());
            assert!(state.active_executions().await.is_empty());
            assert!(state.execution_history().await.is_empty());
            assert!(state.automation_store().await.is_none());
            assert!(!state.is_daemon_running().await);
        });
    }

    #[tokio::test]
    async fn test_set_git_root() {
        let state = AppState::new();
        let path = PathBuf::from("/test/path");

        state.set_git_root(path.clone()).await;

        assert_eq!(state.git_root().await, Some(path));
    }

    #[tokio::test]
    async fn test_start_execution() {
        let state = AppState::new();
        let config = ExecutionConfig::new("test-prompt".to_string());

        let id = state.start_execution(config).await.unwrap();

        let active = state.active_executions().await;
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].id(), &id);
    }

    #[tokio::test]
    async fn test_cancel_execution() {
        let state = AppState::new();
        let config = ExecutionConfig::new("test-prompt".to_string());

        let id = state.start_execution(config).await.unwrap();
        assert!(!state.get_execution(&id).await.unwrap().is_cancelled());

        let cancelled = state.cancel_execution(&id).await.unwrap();
        assert!(cancelled);
        assert!(state.get_execution(&id).await.unwrap().is_cancelled());

        // Cancelling non-existent execution returns false
        let fake_id = ExecutionId::from_string("fake-id".to_string());
        assert!(!state.cancel_execution(&fake_id).await.unwrap());
    }

    #[tokio::test]
    async fn test_finish_execution() {
        let state = AppState::new();
        let config = ExecutionConfig::new("test-prompt".to_string());

        let id = state.start_execution(config).await.unwrap();

        // Execution should be active
        assert_eq!(state.active_executions().await.len(), 1);
        assert!(state.execution_history().await.is_empty());

        // Finish execution
        let finished = state.finish_execution(&id).await.unwrap();
        assert!(finished);

        // Should be in history, not active
        assert!(state.active_executions().await.is_empty());
        assert_eq!(state.execution_history().await.len(), 1);

        // Finishing again returns false
        assert!(!state.finish_execution(&id).await.unwrap());
    }

    #[tokio::test]
    async fn test_daemon_running() {
        let state = AppState::new();

        assert!(!state.is_daemon_running().await);

        state.set_daemon_running(true).await;
        assert!(state.is_daemon_running().await);

        state.set_daemon_running(false).await;
        assert!(!state.is_daemon_running().await);
    }

    #[tokio::test]
    async fn test_available_prompts() {
        let state = AppState::new();

        // Default state has no prompts
        assert!(state.available_prompts().await.is_empty());

        // Add prompts via config
        let mut prompts = BTreeMap::new();
        prompts.insert(
            "test-prompt".to_string(),
            PromptDef::Inline("test template".to_string()),
        );

        let mut config = FileConfig::default();
        config.prompts = prompts;

        *state.merged_config.write().await = config;

        let prompts = state.available_prompts().await;
        assert_eq!(prompts.len(), 1);
        assert_eq!(
            prompts.get("test-prompt"),
            Some(&"test template".to_string())
        );
    }

    #[tokio::test]
    async fn test_get_prompt() {
        let state = AppState::new();

        // Add a prompt
        let mut prompts = BTreeMap::new();
        prompts.insert(
            "my-prompt".to_string(),
            PromptDef::Inline("my template".to_string()),
        );

        let mut config = FileConfig::default();
        config.prompts = prompts;

        *state.merged_config.write().await = config;

        // Get existing prompt
        let prompt = state.get_prompt("my-prompt").await;
        assert!(prompt.is_some());
        assert_eq!(prompt.unwrap().template(), "my template");

        // Get non-existent prompt
        assert!(state.get_prompt("non-existent").await.is_none());
    }

    #[tokio::test]
    async fn test_all_vars() {
        let state = AppState::new();

        // Default has no vars
        assert!(state.all_vars().await.is_empty());

        // Add vars
        let mut vars = BTreeMap::new();
        vars.insert("key1".to_string(), "value1".to_string());
        vars.insert("key2".to_string(), "value2".to_string());

        let mut config = FileConfig::default();
        config.vars = vars;

        *state.merged_config.write().await = config;

        let vars = state.all_vars().await;
        assert_eq!(vars.len(), 2);
        assert_eq!(vars.get("key1"), Some(&"value1".to_string()));
        assert_eq!(vars.get("key2"), Some(&"value2".to_string()));
    }

    #[tokio::test]
    async fn test_save_config() {
        let state = AppState::new();
        let temp_dir = TempDir::new().expect("tempdir should be created");
        let config_path = temp_dir.path().join("test-config.toml");

        // Create a config to save
        let mut config = FileConfig::default();
        config.vars.insert("test".to_string(), "value".to_string());

        // Save it
        state.save_config(&config, &config_path).await.unwrap();

        // Verify file exists and can be loaded
        assert!(config_path.exists());
        let loaded = FileConfig::load_if_exists(&config_path).unwrap().unwrap();
        assert_eq!(loaded.vars.get("test"), Some(&"value".to_string()));
    }

    #[tokio::test]
    async fn test_load_configs() {
        let state = AppState::new();
        let temp_dir = TempDir::new().expect("tempdir should be created");

        // Create home config
        let home_path = temp_dir.path().join("home.toml");
        let mut home_content = String::from(
            r#"
[vars]
home_var = "from_home"
shared = "home_shared"
"#,
        );
        std::fs::write(&home_path, home_content).expect("home config should be written");

        // Create repo config
        let repo_path = temp_dir.path().join("repo.toml");
        let repo_content = r#"
[vars]
repo_var = "from_repo"
shared = "repo_shared"
"#;
        std::fs::write(&repo_path, repo_content).expect("repo config should be written");

        // Load configs
        state
            .load_configs(Some(home_path), Some(repo_path))
            .await
            .unwrap();

        // Check merged config
        let merged = state.merged_config().await;
        assert_eq!(merged.vars.get("home_var"), Some(&"from_home".to_string()));
        assert_eq!(merged.vars.get("repo_var"), Some(&"from_repo".to_string()));
        // Repo should win for shared var
        assert_eq!(merged.vars.get("shared"), Some(&"repo_shared".to_string()));
    }

    #[tokio::test]
    async fn test_load_configs_only_home() {
        let state = AppState::new();
        let temp_dir = TempDir::new().expect("tempdir should be created");

        let home_path = temp_dir.path().join("home.toml");
        std::fs::write(
            &home_path,
            r#"
[vars]
only_home = "value"
"#,
        )
        .expect("home config should be written");

        state.load_configs(Some(home_path), None).await.unwrap();

        let merged = state.merged_config().await;
        assert_eq!(merged.vars.get("only_home"), Some(&"value".to_string()));
    }

    #[tokio::test]
    async fn test_load_configs_none() {
        let state = AppState::new();

        state.load_configs(None, None).await.unwrap();

        // Should have default config
        let merged = state.merged_config().await;
        assert!(merged.vars.is_empty());
    }

    #[tokio::test]
    async fn test_multiple_executions() {
        let state = AppState::new();

        // Start multiple executions
        let id1 = state
            .start_execution(ExecutionConfig::new("prompt1".to_string()))
            .await
            .unwrap();
        let id2 = state
            .start_execution(ExecutionConfig::new("prompt2".to_string()))
            .await
            .unwrap();
        let id3 = state
            .start_execution(ExecutionConfig::new("prompt3".to_string()))
            .await
            .unwrap();

        // All should be active
        assert_eq!(state.active_executions().await.len(), 3);

        // Finish one
        state.finish_execution(&id2).await.unwrap();
        assert_eq!(state.active_executions().await.len(), 2);
        assert_eq!(state.execution_history().await.len(), 1);

        // Finish the rest
        state.finish_execution(&id1).await.unwrap();
        state.finish_execution(&id3).await.unwrap();
        assert_eq!(state.active_executions().await.len(), 0);
        assert_eq!(state.execution_history().await.len(), 3);
    }

    #[test]
    fn test_active_execution_new() {
        let config = ExecutionConfig::new("test".to_string());
        let execution = ActiveExecution::new(config);

        assert!(!execution.is_cancelled());
        assert_eq!(execution.record.state, velor_core::ExecutionState::Pending);
    }

    #[test]
    fn test_active_execution_cancel() {
        let config = ExecutionConfig::new("test".to_string());
        let execution = ActiveExecution::new(config);

        assert!(!execution.is_cancelled());

        execution.cancel_token.cancel();
        assert!(execution.is_cancelled());
    }

    #[tokio::test]
    async fn test_init_automation_store() {
        let state = AppState::new();
        let temp_dir = TempDir::new().expect("tempdir should be created");
        let db_path = temp_dir.path().join("test.db");

        state.init_automation_store(db_path.clone()).await.unwrap();

        let store = state.automation_store().await;
        assert!(store.is_some());

        // Verify database was created
        assert!(db_path.exists());
    }
}

#[cfg(test)]
mod proptest_tests {
    use super::*;
    use proptest::prelude::*;

    proptest! {
        #[test]
        fn test_execution_id_unique(
            prompt_name in "[a-z]{1,20}",
        ) {
            let rt = tokio::runtime::Runtime::new().unwrap();
            let state = AppState::new();

            let id1 = rt.block_on(async {
                state.start_execution(ExecutionConfig::new(prompt_name.clone()))
                    .await
                    .unwrap()
            });
            let id2 = rt.block_on(async {
                state.start_execution(ExecutionConfig::new(prompt_name.clone()))
                    .await
                    .unwrap()
            });

            prop_assert_ne!(id1, id2);
        }

        #[test]
        fn test_vars_preservation(
            vars in prop::collection::btree_map("[a-z]{1,10}", "[a-z0-9]{1,20}", 0..20)
        ) {
            let rt = tokio::runtime::Runtime::new().unwrap();
            let state = AppState::new();

            let result = rt.block_on(async {
                let mut config = FileConfig::default();
                config.vars = vars.clone();

                *state.merged_config.write().await = config;

                let retrieved = state.all_vars().await;
                retrieved == vars
            });

            prop_assert!(result, "vars should be preserved");
        }

        #[test]
        fn test_prompts_preservation(
            prompts in prop::collection::btree_map(
                "[a-z]{1,10}",
                "[a-zA-Z0-9\\s]{1,100}",
                0..10
            )
        ) {
            let rt = tokio::runtime::Runtime::new().unwrap();
            let state = AppState::new();

            let result = rt.block_on(async {
                let mut prompt_map = BTreeMap::new();
                for (k, v) in &prompts {
                    prompt_map.insert(k.clone(), PromptDef::Inline(v.clone()));
                }

                let mut config = FileConfig::default();
                config.prompts = prompt_map;

                *state.merged_config.write().await = config;

                let retrieved: BTreeMap<String, String> = state.available_prompts()
                    .await
                    .into_iter()
                    .collect();
                retrieved == prompts
            });

            prop_assert!(result, "prompts should be preserved");
        }
    }
}
