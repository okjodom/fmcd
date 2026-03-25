use std::fs::{self, Permissions};
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

use tracing_appender::rolling::{RollingFileAppender, Rotation};
use tracing_subscriber::prelude::*;
use tracing_subscriber::{fmt, EnvFilter, Registry};

pub struct LoggingConfig {
    pub level: String,
    pub console_output: bool,
    pub file_output: bool,
    pub log_dir: PathBuf,
    pub rotation: Rotation,
    pub file_permissions: u32,
    pub max_log_files: Option<usize>,
}

impl Default for LoggingConfig {
    fn default() -> Self {
        Self {
            level: "info".to_string(),
            console_output: true,
            file_output: true,
            log_dir: PathBuf::from("./logs"),
            rotation: Rotation::DAILY,
            file_permissions: 0o640, // rw-r-----: owner read/write, group read, no others
            max_log_files: Some(30), // Keep 30 days of logs by default
        }
    }
}

pub fn init_logging(config: LoggingConfig) -> anyhow::Result<()> {
    // Create log directory if it doesn't exist
    std::fs::create_dir_all(&config.log_dir)?;

    // Set secure permissions on log directory (rwxr-x--- - owner full, group
    // read/execute, no others)
    let dir_permissions = Permissions::from_mode(0o750);
    std::fs::set_permissions(&config.log_dir, dir_permissions)?;

    // Clean up old log files if retention policy is set
    if let Some(max_files) = config.max_log_files {
        cleanup_old_log_files(&config.log_dir, max_files)?;
    }

    // Set up environment filter
    let env_filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(&config.level));

    // Initialize the global subscriber with env filter and layers
    let subscriber = Registry::default().with(env_filter);

    // Apply layers based on what's enabled
    match (config.console_output, config.file_output) {
        (true, true) => {
            let file_appender =
                RollingFileAppender::new(config.rotation, &config.log_dir, "fmcd.log");

            let file_layer = fmt::layer()
                .json()
                .with_writer(file_appender)
                .with_current_span(true)
                .with_span_list(true);

            let console_layer = fmt::layer()
                .pretty()
                .with_thread_ids(true)
                .with_target(true);

            subscriber.with(file_layer).with(console_layer).init();
        }
        (true, false) => {
            let console_layer = fmt::layer()
                .pretty()
                .with_thread_ids(true)
                .with_target(true);

            subscriber.with(console_layer).init();
        }
        (false, true) => {
            let file_appender =
                RollingFileAppender::new(config.rotation, &config.log_dir, "fmcd.log");

            let file_layer = fmt::layer()
                .json()
                .with_writer(file_appender)
                .with_current_span(true)
                .with_span_list(true);

            subscriber.with(file_layer).init();
        }
        (false, false) => {
            return Err(anyhow::anyhow!(
                "At least one output (console or file) must be enabled"
            ));
        }
    }

    Ok(())
}

/// Clean up old log files based on retention policy
fn cleanup_old_log_files(log_dir: &PathBuf, max_files: usize) -> anyhow::Result<()> {
    let mut log_files: Vec<_> = fs::read_dir(log_dir)?
        .filter_map(|entry| {
            let entry = entry.ok()?;
            let path = entry.path();

            // Only process .log files
            if path.is_file() && path.extension().is_some_and(|ext| ext == "log") {
                let metadata = entry.metadata().ok()?;
                let modified = metadata.modified().ok()?;
                Some((path, modified))
            } else {
                None
            }
        })
        .collect();

    // Sort by modification time, newest first
    log_files.sort_by(|a, b| b.1.cmp(&a.1));

    // Remove files beyond the retention limit
    if log_files.len() > max_files {
        for (path, _) in log_files.iter().skip(max_files) {
            if let Err(e) = fs::remove_file(path) {
                eprintln!("Failed to remove old log file {:?}: {}", path, e);
            }
        }
    }

    Ok(())
}
