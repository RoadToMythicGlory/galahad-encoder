//! Streaming supervisor state machine.
//!
//! Auto reconnect is core behaviour, not a toggle. The state machine here is pure
//! so transitions and backoff are unit tested independently of capture / FFmpeg.
//! While the user has not pressed Stop, recoverable failures always route back
//! through `Reconnecting` rather than terminating.

use std::time::Duration;

use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum StreamState {
    Idle,
    Starting,
    Streaming,
    Degraded,
    Reconnecting,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamEvent {
    /// User pressed Start (or operator sent start command).
    Start,
    /// Pipeline came up and frames/audio are flowing.
    Up,
    /// Recoverable problem while live (e.g. dropped-frame spike). Stay live.
    Degrade,
    /// Recoverable problem cleared.
    Recover,
    /// Recoverable failure (process exit, network loss, source vanished).
    RecoverableError,
    /// Non-recoverable failure (bad config, no encoder). Stop trying.
    FatalError,
    /// A scheduled retry attempt is being made.
    RetryTick,
    /// User pressed Stop (or operator sent stop command).
    Stop,
}

/// Exponential backoff with a cap. Attempt is 0-based.
pub fn backoff_delay(attempt: u32) -> Duration {
    const BASE_MS: u64 = 500;
    const CAP_MS: u64 = 10_000;
    let shifted = BASE_MS.saturating_mul(1u64 << attempt.min(20));
    Duration::from_millis(shifted.min(CAP_MS))
}

/// Pure transition function. Returns the next state for a (state, event) pair.
pub fn next_state(state: StreamState, event: StreamEvent) -> StreamState {
    use StreamEvent as E;
    use StreamState as S;

    // Stop always wins and returns to Idle.
    if event == E::Stop {
        return S::Idle;
    }

    match (state, event) {
        // From Idle / Failed, only Start does anything meaningful.
        (S::Idle, E::Start) => S::Starting,
        (S::Failed, E::Start) => S::Starting,

        // Bring-up.
        (S::Starting, E::Up) => S::Streaming,
        (S::Starting, E::RecoverableError) => S::Reconnecting,
        (S::Starting, E::FatalError) => S::Failed,

        // Live.
        (S::Streaming, E::Degrade) => S::Degraded,
        (S::Streaming, E::RecoverableError) => S::Reconnecting,
        (S::Streaming, E::FatalError) => S::Failed,

        // Degraded (still live, quality impaired).
        (S::Degraded, E::Recover) => S::Streaming,
        (S::Degraded, E::RecoverableError) => S::Reconnecting,
        (S::Degraded, E::FatalError) => S::Failed,

        // Reconnecting loop.
        (S::Reconnecting, E::RetryTick) => S::Reconnecting,
        (S::Reconnecting, E::Up) => S::Streaming,
        (S::Reconnecting, E::RecoverableError) => S::Reconnecting,
        (S::Reconnecting, E::FatalError) => S::Failed,

        // Anything else is a no-op.
        (other, _) => other,
    }
}

/// Tracks state + retry attempts for the supervisor loop.
#[derive(Debug, Clone)]
pub struct StreamMachine {
    pub state: StreamState,
    pub attempt: u32,
}

impl Default for StreamMachine {
    fn default() -> Self {
        Self {
            state: StreamState::Idle,
            attempt: 0,
        }
    }
}

impl StreamMachine {
    /// Apply an event, returning the new state. Resets the retry counter when we
    /// reach a healthy `Streaming` state, and increments it on each retry tick.
    pub fn apply(&mut self, event: StreamEvent) -> StreamState {
        let next = next_state(self.state, event);
        match event {
            StreamEvent::RetryTick => self.attempt = self.attempt.saturating_add(1),
            StreamEvent::Stop | StreamEvent::Start => self.attempt = 0,
            _ => {}
        }
        if next == StreamState::Streaming {
            self.attempt = 0;
        }
        self.state = next;
        next
    }

    pub fn current_backoff(&self) -> Duration {
        backoff_delay(self.attempt)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn happy_path_to_streaming() {
        let mut m = StreamMachine::default();
        assert_eq!(m.apply(StreamEvent::Start), StreamState::Starting);
        assert_eq!(m.apply(StreamEvent::Up), StreamState::Streaming);
    }

    #[test]
    fn recoverable_error_reconnects_not_fails() {
        let mut m = StreamMachine::default();
        m.apply(StreamEvent::Start);
        m.apply(StreamEvent::Up);
        assert_eq!(
            m.apply(StreamEvent::RecoverableError),
            StreamState::Reconnecting
        );
        assert_eq!(m.apply(StreamEvent::Up), StreamState::Streaming);
    }

    #[test]
    fn stop_always_returns_to_idle() {
        for state in [
            StreamState::Starting,
            StreamState::Streaming,
            StreamState::Degraded,
            StreamState::Reconnecting,
            StreamState::Failed,
        ] {
            assert_eq!(next_state(state, StreamEvent::Stop), StreamState::Idle);
        }
    }

    #[test]
    fn fatal_error_fails_but_can_restart() {
        let mut m = StreamMachine::default();
        m.apply(StreamEvent::Start);
        assert_eq!(m.apply(StreamEvent::FatalError), StreamState::Failed);
        assert_eq!(m.apply(StreamEvent::Start), StreamState::Starting);
    }

    #[test]
    fn degraded_recovers_to_streaming() {
        let mut m = StreamMachine::default();
        m.apply(StreamEvent::Start);
        m.apply(StreamEvent::Up);
        assert_eq!(m.apply(StreamEvent::Degrade), StreamState::Degraded);
        assert_eq!(m.apply(StreamEvent::Recover), StreamState::Streaming);
    }

    #[test]
    fn backoff_is_exponential_and_capped() {
        assert_eq!(backoff_delay(0), Duration::from_millis(500));
        assert_eq!(backoff_delay(1), Duration::from_millis(1000));
        assert_eq!(backoff_delay(2), Duration::from_millis(2000));
        // Capped at 10s.
        assert_eq!(backoff_delay(20), Duration::from_millis(10_000));
        assert_eq!(backoff_delay(99), Duration::from_millis(10_000));
    }

    #[test]
    fn attempt_resets_on_streaming() {
        let mut m = StreamMachine::default();
        m.apply(StreamEvent::Start);
        m.apply(StreamEvent::RecoverableError);
        m.apply(StreamEvent::RetryTick);
        m.apply(StreamEvent::RetryTick);
        assert_eq!(m.attempt, 2);
        m.apply(StreamEvent::Up);
        assert_eq!(m.attempt, 0);
    }
}
