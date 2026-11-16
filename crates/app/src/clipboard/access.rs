//! The clipboard as the sharing sees it: read the text, write the text.
//! Behind a trait so the logic runs on a fake; the real one is in
//! [`super::system`].

use std::fmt;
use std::time::Duration;

use super::rules::ClipboardContent;

/// How many times a clipboard held by another program is tried.
pub const BUSY_ATTEMPTS: u32 = 5;
/// The wait between those attempts.
pub const BUSY_RETRY_DELAY: Duration = Duration::from_millis(20);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AccessError {
    /// Another program holds the clipboard open: worth trying again.
    Busy,
    Failed(String),
}

impl fmt::Display for AccessError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Busy => f.write_str("the clipboard is in use by another program"),
            Self::Failed(detail) => f.write_str(detail),
        }
    }
}

impl std::error::Error for AccessError {}

/// One attempt at reading or writing the clipboard.
pub trait ClipboardAccess {
    fn read(&self) -> Result<ClipboardContent, AccessError>;
    fn write(&self, text: &str) -> Result<(), AccessError>;
}

/// How long to insist on a busy clipboard.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Retry {
    pub attempts: u32,
    pub delay: Duration,
}

impl Default for Retry {
    fn default() -> Self {
        Self {
            attempts: BUSY_ATTEMPTS,
            delay: BUSY_RETRY_DELAY,
        }
    }
}

impl Retry {
    /// Runs `attempt` until it is not [`AccessError::Busy`] or the attempts
    /// are used up (the last busy answer is then the result).
    pub async fn run<T>(
        self,
        mut attempt: impl FnMut() -> Result<T, AccessError>,
    ) -> Result<T, AccessError> {
        let mut result = attempt();
        for _ in 1..self.attempts {
            if !matches!(result, Err(AccessError::Busy)) {
                break;
            }
            tokio::time::sleep(self.delay).await;
            result = attempt();
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::rc::Rc;

    use super::*;

    const QUICK: Retry = Retry {
        attempts: 3,
        delay: Duration::ZERO,
    };

    /// Answers with `answers` in turn (the last one for ever), counting the
    /// attempts.
    fn scripted<T: Clone>(
        answers: Vec<Result<T, AccessError>>,
    ) -> (impl FnMut() -> Result<T, AccessError>, Rc<Cell<usize>>) {
        let calls = Rc::new(Cell::new(0));
        let counter = calls.clone();
        let attempt = move || {
            let n = counter.get();
            counter.set(n + 1);
            answers[n.min(answers.len() - 1)].clone()
        };
        (attempt, calls)
    }

    #[tokio::test]
    async fn a_free_clipboard_is_used_at_once() {
        let (attempt, calls) = scripted(vec![Ok(1)]);
        assert_eq!(QUICK.run(attempt).await, Ok(1));
        assert_eq!(calls.get(), 1);
    }

    #[tokio::test]
    async fn a_busy_clipboard_is_tried_again() {
        let (attempt, calls) =
            scripted(vec![Err(AccessError::Busy), Err(AccessError::Busy), Ok(7)]);
        assert_eq!(QUICK.run(attempt).await, Ok(7));
        assert_eq!(calls.get(), 3);
    }

    #[tokio::test]
    async fn a_clipboard_that_stays_busy_gives_up() {
        let (attempt, calls) = scripted::<u8>(vec![Err(AccessError::Busy)]);
        assert_eq!(QUICK.run(attempt).await, Err(AccessError::Busy));
        assert_eq!(calls.get(), 3);
    }

    #[tokio::test]
    async fn other_failures_are_not_retried() {
        let failed = AccessError::Failed("no memory".into());
        let (attempt, calls) = scripted::<u8>(vec![Err(failed.clone())]);
        assert_eq!(QUICK.run(attempt).await, Err(failed));
        assert_eq!(calls.get(), 1);
    }
}
