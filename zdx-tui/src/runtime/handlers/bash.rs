use std::path::PathBuf;

use tokio_util::sync::CancellationToken;

use crate::events::UiEvent;

/// Bash execution with cancellation support.
///
/// Returns a result event directly; cancellation is cooperative via token.
pub async fn bash_execution(
    id: String,
    command: String,
    root: PathBuf,
    cancel: Option<CancellationToken>,
) -> UiEvent {
    use zdx_core::core::events::ToolOutput;
    use zdx_core::tools::{ToolContext, bash};

    let cmd = command.clone();
    let result_id = id.clone();

    let ctx = ToolContext::new(root, None);
    let run_fut = bash::run(&cmd, &ctx, None);
    let result = if let Some(cancel) = cancel {
        let cancel_clone = cancel.clone();
        tokio::select! {
            result = run_fut => result,
            _ = cancel_clone.cancelled() => ToolOutput::canceled("Interrupted by user"),
        }
    } else {
        run_fut.await
    };
    UiEvent::BashExecuted {
        id: result_id,
        result,
    }
}
