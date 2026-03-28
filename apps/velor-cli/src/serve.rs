//! Telegram listener runtime for `vel serve`.
//!
//! This module provides a long-running server mode that polls Telegram updates,
//! authorizes and normalizes inbound requests, and executes coding-agent tasks.

use clap::{ArgAction, Args};
use color_eyre::eyre::{WrapErr, eyre};
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};
use teloxide::Bot;
use teloxide::net::Download;
use teloxide::payloads::{GetUpdatesSetters, SendMessageSetters};
use teloxide::prelude::Requester;
use teloxide::requests::Request;
use teloxide::types::{
    AllowedUpdate, ChatId, Message, MessageId, ParseMode, PhotoSize, ReplyParameters, Update,
    UpdateKind,
};
use tokio::sync::{Mutex, Semaphore};
use tracing::{error, info, warn};

use velor_core::{
    AgentEvent, AgentProvider, AgentRunner, FileConfig,
    config::{Defaults, TelegramParseMode},
};

/// Arguments for the `serve` subcommand.
#[derive(Debug, Clone, Args)]
pub struct ServeArgs {
    /// Override config path (defaults to {git_root}/.velor/velor.toml).
    #[arg(short, long)]
    pub config: Option<PathBuf>,

    /// Working directory used for agent executions.
    #[arg(long)]
    pub cwd: Option<PathBuf>,

    /// Telegram long-poll timeout in seconds.
    #[arg(long, default_value_t = 10)]
    pub poll_timeout_secs: u64,

    /// Maximum Telegram updates to fetch per poll.
    #[arg(long, default_value_t = 50)]
    pub poll_limit: u8,

    /// Process existing backlog on startup (default: false).
    #[arg(long, action = ArgAction::SetTrue)]
    pub include_backlog: bool,

    /// Prefix required for text/caption task execution.
    #[arg(long, default_value = "codex:")]
    pub trigger_prefix: String,
}

/// Runtime configuration for `vel serve`.
#[derive(Debug, Clone)]
struct ServeConfig {
    trigger_prefix: String,
    poll_timeout_secs: u64,
    poll_limit: u8,
    include_backlog: bool,
    allowed_chat_ids: HashSet<i64>,
    allowed_user_ids: HashSet<i64>,
    max_photo_bytes: usize,
    max_requests_per_minute: u32,
    max_concurrent_tasks: usize,
    task_timeout: Duration,
    media_dir: PathBuf,
    provider_binary_override: Option<String>,
}

impl ServeConfig {
    fn from_sources(args: &ServeArgs, merged: &FileConfig) -> color_eyre::eyre::Result<Self> {
        let telegram_cfg = merged
            .notifications
            .telegram
            .as_ref()
            .ok_or_else(|| eyre!("Missing [notifications.telegram] configuration"))?;

        if !telegram_cfg.enabled {
            return Err(eyre!(
                "[notifications.telegram].enabled=false; enable it before running `vel serve`"
            ));
        }

        let mut allowed_chat_ids = parse_chat_allowlist(&telegram_cfg.chat_id)?;
        if allowed_chat_ids.is_empty() {
            return Err(eyre!(
                "[notifications.telegram].chat_id is required for allowlisting"
            ));
        }

        let env_chat_ids = parse_optional_allowlist_env("VELOR_TELEGRAM_ALLOWED_CHAT_IDS")?;
        allowed_chat_ids.extend(env_chat_ids);

        let allowed_user_ids = parse_optional_allowlist_env("VELOR_TELEGRAM_ALLOWED_USER_IDS")?;

        let max_photo_bytes = std::env::var("VELOR_TELEGRAM_MAX_PHOTO_BYTES")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .unwrap_or(20 * 1024 * 1024);

        let max_requests_per_minute = std::env::var("VELOR_TELEGRAM_RATE_LIMIT_PER_MINUTE")
            .ok()
            .and_then(|v| v.parse::<u32>().ok())
            .unwrap_or(20);

        let max_concurrent_tasks = std::env::var("VELOR_TELEGRAM_MAX_CONCURRENT")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .unwrap_or(2)
            .max(1);

        let task_timeout_secs = std::env::var("VELOR_TELEGRAM_TASK_TIMEOUT_SECS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(1800)
            .max(5);

        let media_dir = std::env::var("VELOR_MEDIA_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|_| std::env::temp_dir().join("velor-telegram-media"));

        let provider_binary_override = std::env::var("VELOR_PROVIDER_BINARY")
            .ok()
            .filter(|v| !v.trim().is_empty());

        Ok(Self {
            trigger_prefix: args.trigger_prefix.trim().to_string(),
            poll_timeout_secs: args.poll_timeout_secs.clamp(1, 15),
            poll_limit: args.poll_limit.clamp(1, 100),
            include_backlog: args.include_backlog,
            allowed_chat_ids,
            allowed_user_ids,
            max_photo_bytes,
            max_requests_per_minute,
            max_concurrent_tasks,
            task_timeout: Duration::from_secs(task_timeout_secs),
            media_dir,
            provider_binary_override,
        })
    }
}

/// Shared runtime context for Telegram polling and execution.
#[derive(Clone)]
struct ServeContext {
    cfg: ServeConfig,
    bot: Bot,
    defaults: Defaults,
    cwd: PathBuf,
    parse_mode: Option<TelegramParseMode>,
    replay_cache: Arc<Mutex<ReplayCache>>,
    rate_limiter: Arc<Mutex<SlidingWindowRateLimiter>>,
    concurrency: Arc<Semaphore>,
}

impl ServeContext {
    fn resolve_binary(&self, provider: AgentProvider) -> String {
        if let Some(explicit) = &self.cfg.provider_binary_override {
            return explicit.clone();
        }

        match provider {
            AgentProvider::Codex => {
                if self.defaults.binary == "claude-glm" {
                    "codex".to_string()
                } else {
                    self.defaults.binary.clone()
                }
            }
            AgentProvider::Claude => self.defaults.binary.clone(),
        }
    }

    async fn run_agent(
        &self,
        provider: AgentProvider,
        prompt: &str,
        images: &[PathBuf],
        mut on_event: impl FnMut(AgentEvent) + Send,
    ) -> color_eyre::eyre::Result<String> {
        let runner = AgentRunner::from_config(
            provider,
            self.defaults.protocol,
            self.defaults.acp.clone(),
            self.defaults.codex.clone(),
        );

        let binary = self.resolve_binary(provider);
        let permission_mode = self
            .defaults
            .permission_mode
            .clone()
            .unwrap_or_else(|| "acceptEdits".to_string());

        let run = runner
            .run_with_events(
                &binary,
                &permission_mode,
                prompt,
                "telegram",
                &self.cwd,
                images,
                |event| {
                    on_event(event);
                },
            )
            .await?;

        Ok(run.stdout)
    }
}

/// In-memory replay protection cache keyed by Telegram update ID.
#[derive(Debug)]
struct ReplayCache {
    entries: VecDeque<(i32, Instant)>,
    seen: HashSet<i32>,
    ttl: Duration,
}

impl ReplayCache {
    fn new(ttl: Duration) -> Self {
        Self {
            entries: VecDeque::new(),
            seen: HashSet::new(),
            ttl,
        }
    }

    fn insert_if_new(&mut self, update_id: i32, now: Instant) -> bool {
        self.evict_expired(now);
        if self.seen.contains(&update_id) {
            return false;
        }
        self.entries.push_back((update_id, now));
        self.seen.insert(update_id);
        true
    }

    fn evict_expired(&mut self, now: Instant) {
        while let Some((id, ts)) = self.entries.front().copied() {
            if now.duration_since(ts) < self.ttl {
                break;
            }
            self.entries.pop_front();
            self.seen.remove(&id);
        }
    }
}

/// Sliding-window rate limiter keyed by actor/chat identity.
#[derive(Debug)]
struct SlidingWindowRateLimiter {
    window: Duration,
    max_events: u32,
    buckets: HashMap<String, VecDeque<Instant>>,
}

impl SlidingWindowRateLimiter {
    fn new(window: Duration, max_events: u32) -> Self {
        Self {
            window,
            max_events,
            buckets: HashMap::new(),
        }
    }

    fn allow(&mut self, key: &str, now: Instant) -> bool {
        let bucket = self.buckets.entry(key.to_string()).or_default();

        while let Some(ts) = bucket.front().copied() {
            if now.duration_since(ts) < self.window {
                break;
            }
            bucket.pop_front();
        }

        if bucket.len() as u32 >= self.max_events {
            return false;
        }

        bucket.push_back(now);
        true
    }
}

/// Normalized execution request from a Telegram message.
#[derive(Debug, Clone)]
struct NormalizedRequest {
    update_id: i32,
    chat_id: i64,
    user_id: Option<i64>,
    reply_to_message_id: Option<i32>,
    provider: AgentProvider,
    task_text: String,
    photo: Option<TelegramPhotoRef>,
}

/// Lightweight photo reference copied from Telegram message metadata.
#[derive(Debug, Clone)]
struct TelegramPhotoRef {
    file_id: String,
    file_unique_id: String,
    width: u32,
    height: u32,
    file_size: Option<u64>,
}

/// Downloaded and validated photo artifact.
#[derive(Debug, Clone)]
struct DownloadedPhoto {
    path: PathBuf,
    mime_type: String,
    width: u32,
    height: u32,
    file_size: usize,
    file_id: String,
    file_unique_id: String,
}

/// Result of update normalization.
#[derive(Debug, Clone)]
enum NormalizeResult {
    /// Message should be ignored without reply.
    Ignore,
    /// Message should get a usage reply.
    Usage {
        chat_id: i64,
        reply_to_message_id: Option<i32>,
        message: String,
    },
    /// Message contains an executable request.
    Request(NormalizedRequest),
}

/// Runs `vel serve` until interrupted.
#[tracing::instrument(level = "info", skip(home_cfg), ret, err)]
pub async fn run_serve(
    args: ServeArgs,
    home_cfg: FileConfig,
    git_root: PathBuf,
    cwd: PathBuf,
) -> color_eyre::eyre::Result<()> {
    let config_path = args
        .config
        .clone()
        .unwrap_or_else(|| FileConfig::default_config_path(&git_root));
    let repo_cfg = FileConfig::load_if_exists(&config_path)
        .wrap_err_with(|| format!("failed to load config at {}", config_path.display()))?
        .unwrap_or_default();
    let merged_cfg = FileConfig::merge(home_cfg, repo_cfg);

    let serve_cfg = ServeConfig::from_sources(&args, &merged_cfg)?;

    let service_cwd = args
        .cwd
        .unwrap_or(cwd)
        .canonicalize()
        .wrap_err("failed to canonicalize serve working directory")?;

    tokio::fs::create_dir_all(&serve_cfg.media_dir)
        .await
        .wrap_err("Failed to create media directory")?;

    let tg_cfg = merged_cfg
        .notifications
        .telegram
        .clone()
        .ok_or_else(|| eyre!("Missing [notifications.telegram] config"))?;

    let token_env = tg_cfg.bot_token_env.clone();
    let bot_token = std::env::var(&token_env).wrap_err_with(|| {
        format!(
            "Missing Telegram token env var '{}' referenced by [notifications.telegram].bot_token_env",
            token_env
        )
    })?;

    let mut bot = Bot::new(bot_token);
    if let Some(api_base_url) = tg_cfg.api_base_url.clone() {
        let parsed = reqwest::Url::parse(&api_base_url)
            .wrap_err_with(|| format!("invalid Telegram api_base_url: {api_base_url}"))?;
        bot = bot.set_api_url(parsed);
    }

    let codex_binary = if let Some(explicit) = serve_cfg.provider_binary_override.clone() {
        explicit
    } else if merged_cfg.defaults.binary == "claude-glm" {
        "codex".to_string()
    } else {
        merged_cfg.defaults.binary.clone()
    };
    velor_core::agent::require_agent_on_path(&codex_binary)?;

    let ctx = Arc::new(ServeContext {
        cfg: serve_cfg.clone(),
        bot,
        defaults: merged_cfg.defaults.clone(),
        cwd: service_cwd,
        parse_mode: tg_cfg.parse_mode,
        replay_cache: Arc::new(Mutex::new(ReplayCache::new(Duration::from_secs(15 * 60)))),
        rate_limiter: Arc::new(Mutex::new(SlidingWindowRateLimiter::new(
            Duration::from_secs(60),
            serve_cfg.max_requests_per_minute,
        ))),
        concurrency: Arc::new(Semaphore::new(serve_cfg.max_concurrent_tasks)),
    });

    info!(
        cwd = %ctx.cwd.display(),
        trigger_prefix = %ctx.cfg.trigger_prefix,
        allowed_chat_ids = ?ctx.cfg.allowed_chat_ids,
        allowed_user_ids = ?ctx.cfg.allowed_user_ids,
        max_concurrent = ctx.cfg.max_concurrent_tasks,
        timeout_secs = ctx.cfg.task_timeout.as_secs(),
        "Starting vel serve telegram polling loop"
    );

    let local = tokio::task::LocalSet::new();
    local
        .run_until(async move {
            if !ctx.cfg.include_backlog {
                let latest = drain_backlog_offset(&ctx).await?;
                if let Some(offset) = latest {
                    info!(offset, "Skipping existing Telegram backlog");
                    polling_loop(ctx, Some(offset)).await?;
                } else {
                    polling_loop(ctx, None).await?;
                }
            } else {
                polling_loop(ctx, None).await?;
            }
            Ok::<(), color_eyre::eyre::Report>(())
        })
        .await?;

    Ok(())
}

async fn fetch_updates(
    ctx: &ServeContext,
    offset: Option<i32>,
    timeout_secs: u64,
    limit: u8,
) -> color_eyre::eyre::Result<Vec<Update>> {
    let mut request = ctx
        .bot
        .get_updates()
        .timeout(u32::try_from(timeout_secs).unwrap_or(u32::MAX))
        .limit(limit)
        .allowed_updates(vec![
            AllowedUpdate::Message,
            AllowedUpdate::EditedMessage,
            AllowedUpdate::ChannelPost,
        ]);

    if let Some(offset) = offset {
        request = request.offset(offset);
    }

    request.send().await.wrap_err("Telegram getUpdates failed")
}

async fn drain_backlog_offset(ctx: &Arc<ServeContext>) -> color_eyre::eyre::Result<Option<i32>> {
    let mut next_offset: Option<i32> = None;

    loop {
        let updates = fetch_updates(ctx, next_offset, 0, 100)
            .await
            .wrap_err("failed to read initial Telegram backlog")?;

        if updates.is_empty() {
            return Ok(next_offset);
        }

        if let Some(last) = updates.last() {
            let update_id = safe_update_id(last.id.0);
            next_offset = Some(update_id.saturating_add(1));
        }

        if updates.len() < 100 {
            return Ok(next_offset);
        }
    }
}

async fn polling_loop(
    ctx: Arc<ServeContext>,
    mut offset: Option<i32>,
) -> color_eyre::eyre::Result<()> {
    let shutdown = tokio::signal::ctrl_c();
    tokio::pin!(shutdown);

    loop {
        let updates_fut =
            fetch_updates(&ctx, offset, ctx.cfg.poll_timeout_secs, ctx.cfg.poll_limit);

        tokio::select! {
            _ = &mut shutdown => {
                info!("Received shutdown signal; stopping vel serve");
                break;
            }
            result = updates_fut => {
                let updates = match result {
                    Ok(items) => items,
                    Err(err) => {
                        warn!(error = ?err, "Telegram polling failed; retrying soon");
                        tokio::time::sleep(Duration::from_secs(3)).await;
                        continue;
                    }
                };

                for update in updates {
                    let update_id = safe_update_id(update.id.0);
                    offset = Some(update_id.saturating_add(1));
                    if let Err(err) = handle_update(Arc::clone(&ctx), update).await {
                        warn!(error = %err, "Failed to process Telegram update");
                    }
                }
            }
        }
    }

    Ok(())
}

async fn handle_update(ctx: Arc<ServeContext>, update: Update) -> color_eyre::eyre::Result<()> {
    let update_id = safe_update_id(update.id.0);
    let now = Instant::now();

    {
        let mut replay = ctx.replay_cache.lock().await;
        if !replay.insert_if_new(update_id, now) {
            info!(update_id, "Duplicate Telegram update ignored");
            return Ok(());
        }
    }

    match normalize_update(update, &ctx.cfg.trigger_prefix) {
        NormalizeResult::Ignore => Ok(()),
        NormalizeResult::Usage {
            chat_id,
            reply_to_message_id,
            message,
        } => {
            if ctx.cfg.allowed_chat_ids.contains(&chat_id)
                && let Err(err) = send_telegram_message(
                    &ctx.bot,
                    ctx.parse_mode,
                    chat_id,
                    reply_to_message_id,
                    &message,
                )
                .await
            {
                warn!(error = %err, "Failed to send Telegram usage message");
            }
            Ok(())
        }
        NormalizeResult::Request(request) => {
            if !is_authorized(&ctx.cfg, request.chat_id, request.user_id) {
                warn!(
                    chat_id = request.chat_id,
                    user_id = ?request.user_id,
                    "Rejected unauthorized Telegram requester"
                );
                return Ok(());
            }

            let rate_key = format!(
                "chat:{}:user:{}",
                request.chat_id,
                request.user_id.unwrap_or_default()
            );
            {
                let mut limiter = ctx.rate_limiter.lock().await;
                if !limiter.allow(&rate_key, now) {
                    warn!(
                        chat_id = request.chat_id,
                        user_id = ?request.user_id,
                        "Rate limit exceeded for Telegram requester"
                    );
                    let _ = send_telegram_message(
                        &ctx.bot,
                        ctx.parse_mode,
                        request.chat_id,
                        request.reply_to_message_id,
                        "Rate limit exceeded. Please retry in a minute.",
                    )
                    .await;
                    return Ok(());
                }
            }

            tokio::task::spawn_local(async move {
                if let Err(err) = process_normalized_request(ctx, request).await {
                    error!(error = %err, "Failed to process normalized Telegram request");
                }
            });

            Ok(())
        }
    }
}

async fn process_normalized_request(
    ctx: Arc<ServeContext>,
    request: NormalizedRequest,
) -> color_eyre::eyre::Result<()> {
    let _permit = ctx
        .concurrency
        .acquire()
        .await
        .map_err(|_| eyre!("Concurrency semaphore closed"))?;

    let task_id = format!(
        "tg-{}-{}",
        request.update_id,
        chrono::Utc::now().timestamp_millis()
    );
    info!(
        task_id,
        update_id = request.update_id,
        chat_id = request.chat_id,
        user_id = ?request.user_id,
        provider = ?request.provider,
        "Accepted Telegram execution request"
    );

    let _ = send_telegram_message(
        &ctx.bot,
        ctx.parse_mode,
        request.chat_id,
        request.reply_to_message_id,
        &format!("Task `{task_id}` accepted. Starting execution..."),
    )
    .await;

    let mut downloaded_images = Vec::new();
    let mut image_paths = Vec::new();

    if let Some(photo) = request.photo.as_ref() {
        match download_photo(&ctx, photo, &task_id).await {
            Ok(downloaded) => {
                image_paths.push(downloaded.path.clone());
                downloaded_images.push(downloaded);
            }
            Err(err) => {
                let _ = send_telegram_message(
                    &ctx.bot,
                    ctx.parse_mode,
                    request.chat_id,
                    request.reply_to_message_id,
                    &format!("Task `{task_id}` failed while downloading photo: {err}"),
                )
                .await;
                return Err(err);
            }
        }
    }

    let media_context = if downloaded_images.is_empty() {
        String::new()
    } else {
        let rows = downloaded_images
            .iter()
            .map(|img| {
                format!(
                    "- file_id={} unique_id={} mime={} size={} bytes dims={}x{} path={}",
                    img.file_id,
                    img.file_unique_id,
                    img.mime_type,
                    img.file_size,
                    img.width,
                    img.height,
                    img.path.display()
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        format!("\n\nImage context:\n{rows}\n")
    };

    let prompt = if media_context.is_empty() {
        request.task_text.clone()
    } else {
        format!("{}{}", request.task_text, media_context)
    };

    let (progress_tx, mut progress_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    let progress_ctx = Arc::clone(&ctx);
    let progress_chat = request.chat_id;
    let progress_reply = request.reply_to_message_id;
    let progress_task = task_id.clone();

    let progress_task_handle = tokio::spawn(async move {
        let mut last_sent = Instant::now() - Duration::from_secs(10);
        while let Some(message) = progress_rx.recv().await {
            if last_sent.elapsed() < Duration::from_secs(3) {
                continue;
            }
            last_sent = Instant::now();
            let text = format!("Task `{}`: {}", progress_task, message);
            let _ = send_telegram_message(
                &progress_ctx.bot,
                progress_ctx.parse_mode,
                progress_chat,
                progress_reply,
                &text,
            )
            .await;
        }
    });

    let progress_tx_for_runner = progress_tx.clone();
    let run = ctx.run_agent(request.provider, &prompt, &image_paths, move |event| {
        let message = match event {
            AgentEvent::Status { message } => Some(message),
            AgentEvent::ToolCall { tool, detail } => Some(format!("tool start: {tool} ({detail})")),
            AgentEvent::ToolResult {
                tool,
                detail,
                success,
            } => Some(format!(
                "tool result: {tool} success={} ({detail})",
                success.unwrap_or(true)
            )),
            AgentEvent::Usage {
                input_tokens,
                output_tokens,
                ..
            } => Some(format!(
                "usage: input={} output={}",
                input_tokens.unwrap_or(0),
                output_tokens.unwrap_or(0)
            )),
            AgentEvent::Error { message } => Some(format!("error: {message}")),
            AgentEvent::TextDelta { .. } => None,
        };

        if let Some(message) = message {
            let _ = progress_tx_for_runner.send(message);
        }
    });

    let result = tokio::time::timeout(ctx.cfg.task_timeout, run).await;

    drop(progress_tx);
    if let Err(err) = progress_task_handle.await {
        warn!(error = %err, "Progress task join error");
    }

    let send_result = match result {
        Ok(Ok(output)) => {
            let final_text = summarize_output_for_telegram(&task_id, &output);
            send_telegram_message(
                &ctx.bot,
                ctx.parse_mode,
                request.chat_id,
                request.reply_to_message_id,
                &final_text,
            )
            .await
        }
        Ok(Err(err)) => {
            send_telegram_message(
                &ctx.bot,
                ctx.parse_mode,
                request.chat_id,
                request.reply_to_message_id,
                &format!("Task `{task_id}` failed: {err}"),
            )
            .await
        }
        Err(_) => {
            send_telegram_message(
                &ctx.bot,
                ctx.parse_mode,
                request.chat_id,
                request.reply_to_message_id,
                &format!(
                    "Task `{task_id}` timed out after {}s",
                    ctx.cfg.task_timeout.as_secs()
                ),
            )
            .await
        }
    };

    if let Err(err) = send_result {
        warn!(task_id, error = %err, "Failed to deliver Telegram final reply");
    }

    for image in downloaded_images {
        if let Err(err) = tokio::fs::remove_file(&image.path).await {
            warn!(
                task_id,
                path = %image.path.display(),
                error = %err,
                "Failed to remove temporary photo"
            );
        }
    }

    Ok(())
}

async fn download_photo(
    ctx: &ServeContext,
    photo: &TelegramPhotoRef,
    task_id: &str,
) -> color_eyre::eyre::Result<DownloadedPhoto> {
    let file = ctx
        .bot
        .get_file(photo.file_id.clone())
        .send()
        .await
        .wrap_err("Telegram getFile failed")?;

    let remote_size = usize::try_from(file.size)
        .unwrap_or(usize::MAX)
        .max(photo.file_size.unwrap_or(0) as usize);

    if remote_size > ctx.cfg.max_photo_bytes {
        return Err(eyre!(
            "Photo exceeds size limit: {} bytes > {} bytes",
            remote_size,
            ctx.cfg.max_photo_bytes
        ));
    }

    let mut bytes = Vec::new();
    ctx.bot
        .download_file(&file.path, &mut bytes)
        .await
        .wrap_err("Failed to download Telegram photo")?;

    if bytes.len() > ctx.cfg.max_photo_bytes {
        return Err(eyre!(
            "Downloaded photo exceeds size limit: {} bytes > {} bytes",
            bytes.len(),
            ctx.cfg.max_photo_bytes
        ));
    }

    let detected = infer::get(&bytes).ok_or_else(|| eyre!("Unable to detect photo MIME type"))?;
    if !detected.mime_type().starts_with("image/") {
        return Err(eyre!(
            "Unsupported attachment type '{}'; only images are allowed",
            detected.mime_type()
        ));
    }

    tokio::fs::create_dir_all(&ctx.cfg.media_dir)
        .await
        .wrap_err("Failed to create media directory")?;

    let filename = format!(
        "{}-{}.{}",
        task_id,
        photo.file_unique_id,
        detected.extension()
    );
    let path = ctx.cfg.media_dir.join(filename);

    tokio::fs::write(&path, &bytes)
        .await
        .wrap_err("Failed to persist downloaded photo")?;

    Ok(DownloadedPhoto {
        path,
        mime_type: detected.mime_type().to_string(),
        width: photo.width,
        height: photo.height,
        file_size: bytes.len(),
        file_id: photo.file_id.to_string(),
        file_unique_id: photo.file_unique_id.clone(),
    })
}

async fn send_telegram_message(
    bot: &Bot,
    parse_mode: Option<TelegramParseMode>,
    chat_id: i64,
    reply_to_message_id: Option<i32>,
    text: &str,
) -> color_eyre::eyre::Result<()> {
    let mut base = text.trim().to_string();
    if base.is_empty() {
        base = "(empty response)".to_string();
    }

    let max_len = 4000usize;
    if base.len() > max_len {
        let idx = base.floor_char_boundary(max_len);
        base = format!("{}\n…[truncated]", &base[..idx]);
    }

    let (msg, mode) = format_outbound_message(&base, parse_mode);

    let mut request = bot.send_message(ChatId(chat_id), msg);

    if let Some(reply_to_message_id) = reply_to_message_id {
        request = request.reply_parameters(ReplyParameters::new(MessageId(reply_to_message_id)));
    }

    if let Some(mode) = mode {
        request = request.parse_mode(mode);
    }

    request
        .send()
        .await
        .wrap_err("Telegram sendMessage failed")?;

    Ok(())
}

fn format_outbound_message(
    text: &str,
    parse_mode: Option<TelegramParseMode>,
) -> (String, Option<ParseMode>) {
    match parse_mode {
        Some(TelegramParseMode::MarkdownV2) => {
            (escape_markdown_v2(text), Some(ParseMode::MarkdownV2))
        }
        Some(TelegramParseMode::Html) => (escape_html(text), Some(ParseMode::Html)),
        None => (text.to_string(), None),
    }
}

fn escape_markdown_v2(text: &str) -> String {
    const RESERVED: &[char] = &[
        '_', '*', '[', ']', '(', ')', '~', '`', '>', '#', '+', '-', '=', '|', '{', '}', '.', '!',
        '\\',
    ];
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        if RESERVED.contains(&ch) {
            out.push('\\');
        }
        out.push(ch);
    }
    out
}

fn escape_html(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn summarize_output_for_telegram(task_id: &str, output: &str) -> String {
    let max_len = 3800usize;
    let trimmed = output.trim();
    if trimmed.is_empty() {
        return format!("Task `{task_id}` completed with no textual output.");
    }

    let body = if trimmed.len() > max_len {
        let idx = trimmed.floor_char_boundary(max_len);
        format!("{}\n…[truncated]", &trimmed[..idx])
    } else {
        trimmed.to_string()
    };

    format!("Task `{task_id}` completed.\n\n{body}")
}

fn normalize_update(update: Update, trigger_prefix: &str) -> NormalizeResult {
    let update_id = safe_update_id(update.id.0);
    match update.kind {
        UpdateKind::Message(message)
        | UpdateKind::EditedMessage(message)
        | UpdateKind::ChannelPost(message) => normalize_message(update_id, message, trigger_prefix),
        _ => NormalizeResult::Ignore,
    }
}

fn normalize_message(update_id: i32, message: Message, trigger_prefix: &str) -> NormalizeResult {
    let candidate_text = message
        .text()
        .map(str::to_string)
        .or_else(|| message.caption().map(str::to_string));

    let photo = message.photo().and_then(select_best_photo);

    let parsed = candidate_text
        .as_deref()
        .and_then(|text| parse_triggered_task(text, trigger_prefix));

    if parsed.is_none() && photo.is_none() {
        return NormalizeResult::Ignore;
    }

    if parsed.is_none() && candidate_text.is_some() {
        return NormalizeResult::Usage {
            chat_id: message.chat.id.0,
            reply_to_message_id: Some(message.id.0),
            message: format!(
                "Prefix commands with `{}` to run Codex. Example: `{} create telegram-test.txt with hello`",
                trigger_prefix, trigger_prefix
            ),
        };
    }

    let Some(task_text) = parsed.or_else(|| {
        if photo.is_some() {
            Some("Please analyze the attached image and explain key observations.".to_string())
        } else {
            None
        }
    }) else {
        return NormalizeResult::Ignore;
    };

    NormalizeResult::Request(NormalizedRequest {
        update_id,
        chat_id: message.chat.id.0,
        user_id: message
            .from
            .as_ref()
            .and_then(|user| i64::try_from(user.id.0).ok()),
        reply_to_message_id: Some(message.id.0),
        provider: AgentProvider::Codex,
        task_text,
        photo,
    })
}

fn parse_triggered_task(text: &str, trigger_prefix: &str) -> Option<String> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return None;
    }

    if let Some(rest) = strip_prefix_case_insensitive(trimmed, trigger_prefix) {
        let task = rest.trim();
        if !task.is_empty() {
            return Some(task.to_string());
        }
    }

    if let Some(rest) = strip_prefix_case_insensitive(trimmed, "/codex") {
        let task = rest.trim_start_matches(':').trim();
        if !task.is_empty() {
            return Some(task.to_string());
        }
    }

    None
}

fn strip_prefix_case_insensitive<'a>(text: &'a str, prefix: &str) -> Option<&'a str> {
    let prefix = prefix.trim();
    if prefix.is_empty() {
        return None;
    }

    let mut consumed_bytes = 0usize;
    let mut actual_chars = text.chars();
    for expected in prefix.chars() {
        let actual = actual_chars.next()?;
        if !actual.eq_ignore_ascii_case(&expected) {
            return None;
        }
        consumed_bytes += actual.len_utf8();
    }

    Some(&text[consumed_bytes..])
}

fn select_best_photo(photos: &[PhotoSize]) -> Option<TelegramPhotoRef> {
    photos
        .iter()
        .max_by_key(|p| {
            let area = u64::from(p.width) * u64::from(p.height);
            let size = u64::from(p.file.size);
            (area, size)
        })
        .map(|photo| TelegramPhotoRef {
            file_id: photo.file.id.clone(),
            file_unique_id: photo.file.unique_id.to_string(),
            width: photo.width,
            height: photo.height,
            file_size: Some(u64::from(photo.file.size)),
        })
}

fn is_authorized(config: &ServeConfig, chat_id: i64, user_id: Option<i64>) -> bool {
    let chat_allowed = config.allowed_chat_ids.contains(&chat_id);

    let user_allowed = if config.allowed_user_ids.is_empty() {
        true
    } else {
        user_id
            .map(|id| config.allowed_user_ids.contains(&id))
            .unwrap_or(false)
    };

    chat_allowed && user_allowed
}

fn parse_chat_allowlist(chat_id_field: &str) -> color_eyre::eyre::Result<HashSet<i64>> {
    let mut out = HashSet::new();
    for item in chat_id_field
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        let parsed = item.parse::<i64>().map_err(|e| {
            eyre!(
                "Invalid chat_id '{}' in [notifications.telegram]: {}",
                item,
                e
            )
        })?;
        out.insert(parsed);
    }
    Ok(out)
}

fn parse_optional_allowlist_env(env_name: &str) -> color_eyre::eyre::Result<HashSet<i64>> {
    let Some(raw) = std::env::var(env_name).ok() else {
        return Ok(HashSet::new());
    };

    let mut out = HashSet::new();
    for item in raw.split(',').map(str::trim).filter(|s| !s.is_empty()) {
        let parsed = item
            .parse::<i64>()
            .map_err(|e| eyre!("Invalid ID '{}' in {}: {}", item, env_name, e))?;
        out.insert(parsed);
    }
    Ok(out)
}

fn safe_update_id(raw: u32) -> i32 {
    i32::try_from(raw).unwrap_or(i32::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replay_cache_rejects_duplicates() {
        let now = Instant::now();
        let mut cache = ReplayCache::new(Duration::from_secs(30));

        assert!(
            cache.insert_if_new(1, now),
            "first insert should be accepted"
        );
        assert!(
            !cache.insert_if_new(1, now + Duration::from_secs(1)),
            "duplicate insert should be rejected"
        );
    }

    #[test]
    fn replay_cache_evicts_after_ttl() {
        let now = Instant::now();
        let mut cache = ReplayCache::new(Duration::from_secs(10));
        assert!(cache.insert_if_new(7, now));
        assert!(cache.insert_if_new(7, now + Duration::from_secs(11)));
    }

    #[test]
    fn rate_limiter_enforces_window_limit() {
        let now = Instant::now();
        let mut limiter = SlidingWindowRateLimiter::new(Duration::from_secs(60), 2);
        assert!(limiter.allow("k", now));
        assert!(limiter.allow("k", now + Duration::from_secs(1)));
        assert!(!limiter.allow("k", now + Duration::from_secs(2)));
        assert!(limiter.allow("k", now + Duration::from_secs(61)));
    }

    #[test]
    fn parse_trigger_supports_codex_prefix() {
        let parsed = parse_triggered_task("codex: create file", "codex:");
        assert_eq!(parsed, Some("create file".to_string()));
    }

    #[test]
    fn parse_trigger_supports_slash_codex() {
        let parsed = parse_triggered_task("/codex summarize repo", "codex:");
        assert_eq!(parsed, Some("summarize repo".to_string()));
    }

    #[test]
    fn parse_trigger_requires_non_empty_task() {
        assert_eq!(parse_triggered_task("codex:", "codex:"), None);
    }

    #[test]
    fn strip_prefix_case_insensitive_matches_ascii() {
        let out = strip_prefix_case_insensitive("CoDeX: do x", "codex:");
        assert_eq!(out, Some(" do x"));
    }

    #[test]
    fn is_authorized_checks_chat_and_user_allowlists() {
        let cfg = ServeConfig {
            trigger_prefix: "codex:".to_string(),
            poll_timeout_secs: 30,
            poll_limit: 50,
            include_backlog: false,
            allowed_chat_ids: HashSet::from([1]),
            allowed_user_ids: HashSet::from([2]),
            max_photo_bytes: 1,
            max_requests_per_minute: 1,
            max_concurrent_tasks: 1,
            task_timeout: Duration::from_secs(1),
            media_dir: PathBuf::from("/tmp"),
            provider_binary_override: None,
        };

        assert!(is_authorized(&cfg, 1, Some(2)));
        assert!(!is_authorized(&cfg, 1, Some(3)));
        assert!(!is_authorized(&cfg, 9, Some(2)));
    }

    #[test]
    fn parse_chat_allowlist_parses_comma_list() {
        let parsed = parse_chat_allowlist("-1001,42");
        assert!(parsed.is_ok());
        let set = parsed.unwrap_or_default();
        assert!(set.contains(&-1001));
        assert!(set.contains(&42));
    }

    #[test]
    fn summarize_output_truncates_long_payload() {
        let long = "a".repeat(5000);
        let msg = summarize_output_for_telegram("task", &long);
        assert!(msg.contains("truncated"));
        assert!(msg.len() < 4300);
    }
}
