//! Steering in Telegram topics: messages added to the running turn of their
//! topic at its next safe point (SPEC §Steering).
//!
//! Agent topics run in this process, so their running turn's inbox lives
//! here ([`BotSteer::open`]). Orchestrator topics run in the orchestrator
//! service, which admits steers itself (`Delivery::Steer`). Both share the
//! acknowledgement state: "will be added", edited to "added" when the turn
//! reads the message, whichever comes first.

use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use zdx_engine::core::steer::{SOURCE_USER, SteerInbox, SteerItem};

use crate::types::IncomingMessage;

/// Steer ids of agent-topic inboxes start with this, so they never match a
/// service input id (plain digits).
const LOCAL_PREFIX: &str = "b";

pub(crate) const ACK_PENDING: &str = "↪️ Will be added to the running turn";
pub(crate) const ACK_ADDED: &str = "↪️ Added to the running turn";
pub(crate) const ACK_NEW_TURN: &str = "↪️ The turn ended first, so this runs as the next turn";
pub(crate) const ACK_NOT_ADDED: &str = "↩️ Not added: the turn was cancelled";

/// A steer accepted by an agent topic's running turn, kept until it is read
/// or settled when the turn ends.
pub(crate) struct LocalSteer {
    pub incoming: IncomingMessage,
    pub reply_to_message_id: Option<i64>,
    pub topic_id: Option<i64>,
}

impl std::fmt::Debug for LocalSteer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LocalSteer")
            .field("message_id", &self.incoming.message_id)
            .finish_non_exhaustive()
    }
}

struct ActiveTurn {
    inbox: SteerInbox,
    pending: HashMap<String, LocalSteer>,
}

/// Where an acknowledgement was posted.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Ack {
    pub chat: i64,
    pub message_id: i64,
}

/// One steer's acknowledgement: where it was posted, and an outcome that
/// arrived before it was posted.
#[derive(Default)]
struct AckEntry {
    ack: Option<Ack>,
    outcome: Option<&'static str>,
}

/// Steers of a closed agent turn.
pub(crate) struct ClosedTurn {
    /// Never read, in arrival order.
    pub unread: Vec<(String, LocalSteer)>,
    /// Acknowledgements of steers the turn read whose "added" edit is still
    /// due (the turn's events stopped being read, for example after a cancel).
    pub added: Vec<Ack>,
}

#[derive(Default)]
pub(crate) struct BotSteer {
    next_id: AtomicU64,
    active: Mutex<HashMap<String, ActiveTurn>>,
    acks: Mutex<HashMap<String, AckEntry>>,
}

impl BotSteer {
    /// Registers the inbox of an agent turn about to run on `thread_id`.
    pub(crate) fn open(&self, thread_id: &str) -> SteerInbox {
        let inbox = SteerInbox::new();
        self.active
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(
                thread_id.to_string(),
                ActiveTurn {
                    inbox: inbox.clone(),
                    pending: HashMap::new(),
                },
            );
        inbox
    }

    /// Adds `text` to the running agent turn of `thread_id`. Gives the steer
    /// back when no turn runs there or its inbox is closed.
    pub(crate) fn try_push(
        &self,
        thread_id: &str,
        text: String,
        steer: LocalSteer,
    ) -> Result<String, Box<LocalSteer>> {
        let mut active = self
            .active
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(turn) = active.get_mut(thread_id) else {
            return Err(Box::new(steer));
        };
        let id = format!(
            "{LOCAL_PREFIX}{}",
            self.next_id.fetch_add(1, Ordering::Relaxed)
        );
        let item = SteerItem {
            id: id.clone(),
            source: SOURCE_USER.to_string(),
            text,
        };
        if turn.inbox.try_push(item).is_err() {
            return Err(Box::new(steer));
        }
        turn.pending.insert(id.clone(), steer);
        // Known before its acknowledgement is posted, so an outcome that
        // arrives first is kept.
        self.lock_acks().insert(id.clone(), AckEntry::default());
        Ok(id)
    }

    /// Ends the turn's steering: closes its inbox and returns the steers it
    /// never read, plus acknowledgements of read ones still to edit.
    pub(crate) fn close(&self, thread_id: &str) -> ClosedTurn {
        let Some(mut turn) = self
            .active
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(thread_id)
        else {
            return ClosedTurn {
                unread: Vec::new(),
                added: Vec::new(),
            };
        };
        let unread: Vec<(String, LocalSteer)> = turn
            .inbox
            .drain_unread()
            .into_iter()
            .filter_map(|item| turn.pending.remove(&item.id).map(|steer| (item.id, steer)))
            .collect();
        // What is left was read by the turn.
        let added = turn
            .pending
            .keys()
            .filter_map(|id| self.settle(id, ACK_ADDED))
            .collect();
        ClosedTurn { unread, added }
    }

    fn lock_acks(&self) -> std::sync::MutexGuard<'_, HashMap<String, AckEntry>> {
        self.acks
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// The acknowledgement for `id` is about to be posted: from now on an
    /// outcome that arrives first is kept for it.
    pub(crate) fn expect_ack(&self, id: &str) {
        self.lock_acks().entry(id.to_string()).or_default();
    }

    /// Records the acknowledgement posted for `id`. Returns the outcome to
    /// edit it to when one arrived first.
    pub(crate) fn record_ack(&self, id: &str, ack: Ack) -> Option<&'static str> {
        let mut acks = self.lock_acks();
        let entry = acks.entry(id.to_string()).or_default();
        if let Some(outcome) = entry.outcome {
            acks.remove(id);
            return Some(outcome);
        }
        entry.ack = Some(ack);
        None
    }

    /// The acknowledgement for `id` could not be posted.
    pub(crate) fn forget(&self, id: &str) {
        self.lock_acks().remove(id);
    }

    /// Steer `id` reached `outcome`: returns its acknowledgement to edit, or
    /// keeps the outcome until the acknowledgement is posted. An id that is
    /// not known is ignored (not a steer, or already settled).
    pub(crate) fn settle(&self, id: &str, outcome: &'static str) -> Option<Ack> {
        let mut acks = self.lock_acks();
        let entry = acks.get_mut(id)?;
        if let Some(ack) = entry.ack {
            acks.remove(id);
            return Some(ack);
        }
        entry.outcome = Some(outcome);
        None
    }

    /// A service turn read steer `id`. Only steers are injected, so an id
    /// whose acknowledgement is not posted yet is remembered.
    pub(crate) fn settle_service_injection(&self, id: &str) -> Option<Ack> {
        self.lock_acks().entry(id.to_string()).or_default();
        self.settle(id, ACK_ADDED)
    }

    /// Whether `id` names a steer of an agent topic in this process.
    pub(crate) fn is_local(id: &str) -> bool {
        id.starts_with(LOCAL_PREFIX)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn steer() -> LocalSteer {
        LocalSteer {
            incoming: IncomingMessage {
                chat_id: 1,
                message_id: 2,
                user_id: 3,
                text: Some("detail".to_string()),
                images: Vec::new(),
                audios: Vec::new(),
                documents: Vec::new(),
                videos: Vec::new(),
                notices: Vec::new(),
                message_thread_id: Some(4),
                is_forum: true,
                replied_to: None,
            },
            reply_to_message_id: Some(2),
            topic_id: Some(4),
        }
    }

    const ACK: Ack = Ack {
        chat: 1,
        message_id: 9,
    };

    #[test]
    fn steers_reach_only_an_open_turn_and_unread_ones_come_back_in_order() {
        let steer_state = BotSteer::default();
        assert!(
            steer_state.try_push("t", "x".into(), steer()).is_err(),
            "no turn"
        );
        let inbox = steer_state.open("t");
        let first = steer_state.try_push("t", "one".into(), steer()).unwrap();
        assert!(BotSteer::is_local(&first));
        assert!(steer_state.record_ack(&first, ACK).is_none());
        let taken = inbox.take();
        assert_eq!(taken[0].id, first);
        let second = steer_state.try_push("t", "two".into(), steer()).unwrap();
        let third = steer_state.try_push("t", "three".into(), steer()).unwrap();
        inbox.close();
        assert!(
            steer_state.try_push("t", "late".into(), steer()).is_err(),
            "closed"
        );
        let closed = steer_state.close("t");
        assert_eq!(
            closed
                .unread
                .iter()
                .map(|(id, _)| id.as_str())
                .collect::<Vec<_>>(),
            [second.as_str(), third.as_str()]
        );
        // The read one was never marked added while the turn ran.
        assert_eq!(closed.added.len(), 1);
        assert!(steer_state.close("t").unread.is_empty());
    }

    #[test]
    fn an_outcome_before_the_acknowledgement_is_applied_when_it_is_posted() {
        let steer_state = BotSteer::default();
        steer_state.open("t");
        let id = steer_state.try_push("t", "x".into(), steer()).unwrap();
        assert!(steer_state.settle(&id, ACK_NOT_ADDED).is_none());
        assert_eq!(steer_state.record_ack(&id, ACK), Some(ACK_NOT_ADDED));
        assert!(steer_state.settle(&id, ACK_ADDED).is_none(), "settled once");

        assert!(steer_state.settle_service_injection("42").is_none());
        assert_eq!(steer_state.record_ack("42", ACK), Some(ACK_ADDED));
        assert!(steer_state.record_ack("43", ACK).is_none());
        assert_eq!(
            steer_state.settle("43", ACK_NEW_TURN).unwrap().message_id,
            9
        );
        assert!(
            steer_state.settle("44", ACK_NEW_TURN).is_none(),
            "not a steer"
        );
    }
}
