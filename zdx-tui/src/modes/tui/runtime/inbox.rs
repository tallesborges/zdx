use tokio::sync::mpsc;

use crate::modes::tui::events::UiEvent;

/// Sender for the runtime's event inbox.
pub type UiEventSender = mpsc::UnboundedSender<UiEvent>;

/// Receiver for the runtime's event inbox.
pub type UiEventReceiver = mpsc::UnboundedReceiver<UiEvent>;
