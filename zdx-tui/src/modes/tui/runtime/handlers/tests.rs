use std::time::Duration;

use tokio_util::sync::CancellationToken;

#[tokio::test]
async fn test_cancellation_token_stops_handler() {
    tokio::time::pause();
    let token = CancellationToken::new();
    let token_clone = token.clone();

    // Spawn a task that waits for cancellation or sleeps forever
    let handle = tokio::spawn(async move {
        tokio::select! {
            _ = token_clone.cancelled() => "cancelled",
            _ = tokio::time::sleep(Duration::from_secs(60)) => "timeout",
        }
    });

    // Cancel immediately
    token.cancel();

    // Task should return "cancelled" quickly
    let result = tokio::time::timeout(Duration::from_millis(100), handle);
    tokio::time::advance(Duration::from_millis(100)).await;
    let result = result
        .await
        .expect("should complete within timeout")
        .expect("task should not panic");

    assert_eq!(result, "cancelled");
}

#[tokio::test]
async fn test_cancellation_token_clones_are_connected() {
    let original = CancellationToken::new();
    let clone1 = original.clone();
    let clone2 = original.clone();

    // None should be cancelled yet
    assert!(!original.is_cancelled());
    assert!(!clone1.is_cancelled());
    assert!(!clone2.is_cancelled());

    // Cancel the original
    original.cancel();

    // All clones should observe cancellation
    assert!(original.is_cancelled());
    assert!(clone1.is_cancelled());
    assert!(clone2.is_cancelled());
}
