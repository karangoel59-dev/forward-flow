//! Turn identity survives provider rounds and automatic approval batches.
use std::sync::{Arc, Mutex};
use tokio::sync::Notify;

#[derive(Default)]
pub(crate) struct Cancellation {
    cancelled: std::sync::atomic::AtomicBool,
    changed: Notify,
    pub text: Mutex<String>,
}
impl Cancellation {
    pub fn cancel(&self) {
        self.cancelled
            .store(true, std::sync::atomic::Ordering::SeqCst);
        self.changed.notify_waiters();
    }
    pub fn check(&self) -> Result<(), String> {
        if self.cancelled.load(std::sync::atomic::Ordering::SeqCst) {
            Err("Chat stopped".into())
        } else {
            Ok(())
        }
    }
    /// Dropping the losing future closes a provider/MCP request where supported.
    pub async fn run<T>(
        &self,
        work: impl std::future::Future<Output = Result<T, String>>,
    ) -> Result<T, String> {
        let changed = self.changed.notified();
        tokio::pin!(changed);
        changed.as_mut().enable();
        self.check()?;
        tokio::select! {
            biased;
            _ = changed => Err("Chat stopped".into()),
            result = work => { self.check()?; result }
        }
    }
}
#[derive(Default)]
pub(crate) struct Turns {
    active: Option<(String, String, Arc<Cancellation>)>,
    stopped: Vec<String>,
}
impl Turns {
    pub const fn new() -> Self {
        Self {
            active: None,
            stopped: Vec::new(),
        }
    }
    pub fn enter(
        &mut self,
        id: &str,
        notebook: &str,
        begin: bool,
    ) -> Result<Arc<Cancellation>, String> {
        if self.stopped.iter().any(|v| v == id) {
            return Err("Chat stopped".into());
        }
        if let Some((current, scope, token)) = &self.active {
            if current == id && scope == notebook {
                token.check()?;
                return Ok(token.clone());
            }
            if !begin {
                return Err("Chat turn is no longer active".into());
            }
            token.cancel();
        } else if !begin {
            return Err("Chat turn is no longer active".into());
        }
        if let Some((old, _, _)) = &self.active {
            self.stopped.push(old.clone());
        }
        let token = Arc::new(Cancellation::default());
        self.active = Some((id.into(), notebook.into(), token.clone()));
        Ok(token)
    }
    pub fn is_current(&self, id: &str, notebook: &str) -> bool {
        self.active
            .as_ref()
            .is_some_and(|(current, scope, _)| current == id && scope == notebook)
    }
    pub fn invalidate(&mut self) {
        if let Some((id, _, token)) = self.active.take() {
            token.cancel();
            self.stopped.push(id);
        }
    }
    pub fn stop(&mut self, id: &str, notebook: &str) -> Option<Arc<Cancellation>> {
        if !self.stopped.iter().any(|v| v == id) {
            self.stopped.push(id.into());
        }
        if let Some((current, scope, token)) = &self.active {
            if current == id && scope == notebook {
                token.cancel();
                return Some(token.clone());
            }
        }
        None
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn stopped_turn_rejects_queued_actions_and_cannot_be_restarted() {
        let mut turns = Turns::default();
        let old = turns.enter("old", "Ideas", true).unwrap();
        turns.stop("old", "Ideas");
        assert!(old.check().is_err());
        assert!(turns.enter("old", "Ideas", false).is_err());
        assert!(turns.enter("old", "Ideas", true).is_err());
        assert!(turns.enter("new", "Ideas", true).is_ok());
        assert!(old.check().is_err());
        assert!(turns.enter("old", "Ideas", false).is_err());
    }
    #[test]
    fn stop_before_registration_and_replacement_invalidate_late_work() {
        let mut turns = Turns::default();
        turns.stop("queued", "Ideas");
        assert!(turns.enter("queued", "Ideas", true).is_err());
        let old = turns.enter("one", "Ideas", true).unwrap();
        turns.enter("two", "Work", true).unwrap();
        assert!(old.check().is_err());
        assert!(turns.enter("one", "Ideas", false).is_err());
    }
    #[test]
    fn changing_conversation_invalidates_old_token_without_cancelling_new_one() {
        let mut turns = Turns::new();
        let old = turns.enter("old", "Ideas", true).unwrap();
        turns.invalidate();
        let new = turns.enter("new", "Ideas", true).unwrap();
        assert!(!turns.is_current("old", "Ideas"));
        turns.stop("old", "Ideas");
        assert!(old.check().is_err());
        assert!(new.check().is_ok());
        assert!(turns.is_current("new", "Ideas"));
    }
    #[test]
    fn cancellation_drops_in_flight_work() {
        tauri::async_runtime::block_on(async {
            let token = Cancellation::default();
            let result = token
                .run(async {
                    token.cancel();
                    std::future::pending::<Result<(), String>>().await
                })
                .await;
            assert_eq!(result.unwrap_err(), "Chat stopped");
        });
    }
}
