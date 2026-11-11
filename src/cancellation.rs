use std::sync::{Arc, Mutex};
use tokio_util::sync::CancellationToken;

/// Cancellation system for graceful interruption of operations
#[derive(Clone)]
pub struct GhostwriterCancellation {
    /// Main cancellation token for the entire operation
    pub main_token: CancellationToken,

    /// Token specifically for the current execution cycle (wrapped in Mutex for interior mutability)
    pub execution_token: Arc<Mutex<CancellationToken>>,
}

impl GhostwriterCancellation {
    pub fn new() -> Self {
        Self {
            main_token: CancellationToken::new(),
            execution_token: Arc::new(Mutex::new(CancellationToken::new())),
        }
    }

    /// Cancel the current execution cycle (for config changes)
    pub fn cancel_execution(&self) {
        if let Ok(token) = self.execution_token.lock() {
            token.cancel();
        }
    }

    /// Cancel everything (for shutdown)
    pub fn cancel_all(&self) {
        self.main_token.cancel();
        if let Ok(token) = self.execution_token.lock() {
            token.cancel();
        }
    }

    /// Create a new execution token for the next cycle
    pub fn new_execution_cycle(&self) {
        if let Ok(mut token) = self.execution_token.lock() {
            *token = CancellationToken::new();
        }
    }

    /// Check if the main cancellation token has been triggered (for long-lived tasks)
    pub fn should_cancel_main(&self) -> bool {
        self.main_token.is_cancelled()
    }

    /// Get a cancellation token for the current execution
    pub fn execution_token(&self) -> CancellationToken {
        self.execution_token
            .lock()
            .map(|token| token.clone())
            .unwrap_or_else(|_| CancellationToken::new())
    }

    /// Get the main cancellation token
    pub fn main_token(&self) -> CancellationToken {
        self.main_token.clone()
    }
}

impl Default for GhostwriterCancellation {
    fn default() -> Self {
        Self::new()
    }
}

/// Helper for running operations with cancellation support
pub async fn with_cancellation<F, T>(future: F, cancellation: &GhostwriterCancellation) -> anyhow::Result<T>
where
    F: std::future::Future<Output = anyhow::Result<T>>,
{
    let execution_token = cancellation.execution_token();
    let main_token = cancellation.main_token();

    tokio::select! {
        result = future => result,
        _ = execution_token.cancelled() => {
            Err(anyhow::anyhow!("Operation cancelled by execution token"))
        },
        _ = main_token.cancelled() => {
            Err(anyhow::anyhow!("Operation cancelled by main token"))
        }
    }
}
