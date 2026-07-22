//! TUI-local voice-session state and backend seam.
//!
//! The recorder and transcriber remain app singletons.  This module owns only
//! the headless TUI lifecycle, which makes Escape precedence and stale async
//! completion behavior testable without a microphone or network.

use warpui_core::{Entity, ModelContext};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum TuiVoiceInputState {
    #[default]
    Idle,
    Listening,
    Transcribing,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum TuiVoiceInputEvent {
    StateChanged(TuiVoiceInputState),
    Completed { generation: u64, text: String },
    Failed { generation: u64, hint: String },
}

/// Narrow backend contract used by the controller tests. Production wiring
/// adapts the app's existing `VoiceInput`/`VoiceTranscriber` singletons around
/// this lifecycle; tests inject a fake implementation.
#[cfg(test)]
pub(crate) trait TuiVoiceBackend {
    fn start_listening(&mut self) -> Result<(), String>;
    fn stop_listening(&mut self) -> Result<(), String>;
}

/// A backend-agnostic controller used by focused unit tests and as the
/// lifecycle contract for the model below.
#[cfg(test)]
pub(crate) struct TuiVoiceInputController<B> {
    backend: B,
    state: TuiVoiceInputState,
    generation: u64,
}

#[cfg(test)]
impl<B: TuiVoiceBackend> TuiVoiceInputController<B> {
    pub(crate) fn new(backend: B) -> Self {
        Self {
            backend,
            state: TuiVoiceInputState::Idle,
            generation: 0,
        }
    }

    pub(crate) fn state(&self) -> TuiVoiceInputState {
        self.state
    }

    pub(crate) fn generation(&self) -> u64 {
        self.generation
    }

    pub(crate) fn start(&mut self) -> Result<bool, String> {
        if self.state != TuiVoiceInputState::Idle {
            return Ok(false);
        }
        self.backend.start_listening()?;
        self.generation = self.generation.wrapping_add(1);
        self.state = TuiVoiceInputState::Listening;
        Ok(true)
    }

    /// Stops recording for transcription. A second Escape while transcribing
    /// is deliberately a handled no-op and never reaches the backend.
    pub(crate) fn stop(&mut self) -> Result<bool, String> {
        match self.state {
            TuiVoiceInputState::Listening => {
                self.backend.stop_listening()?;
                self.state = TuiVoiceInputState::Transcribing;
                Ok(true)
            }
            TuiVoiceInputState::Transcribing => Ok(true),
            TuiVoiceInputState::Idle => Ok(false),
        }
    }

    pub(crate) fn complete(&mut self, generation: u64, text: String) -> bool {
        if self.state != TuiVoiceInputState::Transcribing || generation != self.generation {
            return false;
        }
        self.state = TuiVoiceInputState::Idle;
        let _ = text;
        true
    }

    pub(crate) fn fail(&mut self, generation: u64) -> bool {
        if generation != self.generation || self.state == TuiVoiceInputState::Idle {
            return false;
        }
        self.state = TuiVoiceInputState::Idle;
        true
    }
}

/// Entity-facing state holder used by the terminal session view. The actual
/// app recorder is intentionally kept outside this model because it is a
/// singleton owned by `warp`; this model gives the view a redraw/event source
/// and a generation token for async completion.
pub(crate) struct TuiVoiceInputModel {
    state: TuiVoiceInputState,
    generation: u64,
}

impl Entity for TuiVoiceInputModel {
    type Event = TuiVoiceInputEvent;
}

impl TuiVoiceInputModel {
    pub(crate) fn new(_ctx: &mut ModelContext<Self>) -> Self {
        Self {
            state: TuiVoiceInputState::Idle,
            generation: 0,
        }
    }

    pub(crate) fn state(&self) -> TuiVoiceInputState {
        self.state
    }

    pub(crate) fn generation(&self) -> u64 {
        self.generation
    }

    pub(crate) fn is_active(&self) -> bool {
        self.state != TuiVoiceInputState::Idle
    }

    pub(crate) fn start(&mut self, ctx: &mut ModelContext<Self>) -> bool {
        if self.state != TuiVoiceInputState::Idle {
            return false;
        }
        self.generation = self.generation.wrapping_add(1);
        self.state = TuiVoiceInputState::Listening;
        ctx.emit(TuiVoiceInputEvent::StateChanged(self.state));
        true
    }

    pub(crate) fn stop(&mut self, ctx: &mut ModelContext<Self>) -> bool {
        if self.state != TuiVoiceInputState::Listening {
            return self.state == TuiVoiceInputState::Transcribing;
        }
        self.state = TuiVoiceInputState::Transcribing;
        ctx.emit(TuiVoiceInputEvent::StateChanged(self.state));
        true
    }

    pub(crate) fn complete(
        &mut self,
        generation: u64,
        text: String,
        ctx: &mut ModelContext<Self>,
    ) -> bool {
        if self.state != TuiVoiceInputState::Transcribing || generation != self.generation {
            return false;
        }
        self.state = TuiVoiceInputState::Idle;
        ctx.emit(TuiVoiceInputEvent::Completed { generation, text });
        ctx.emit(TuiVoiceInputEvent::StateChanged(self.state));
        true
    }

    pub(crate) fn fail(
        &mut self,
        generation: u64,
        hint: String,
        ctx: &mut ModelContext<Self>,
    ) -> bool {
        if generation != self.generation || self.state == TuiVoiceInputState::Idle {
            return false;
        }
        self.state = TuiVoiceInputState::Idle;
        ctx.emit(TuiVoiceInputEvent::Failed { generation, hint });
        ctx.emit(TuiVoiceInputEvent::StateChanged(self.state));
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct FakeBackend {
        starts: usize,
        stops: usize,
    }

    impl TuiVoiceBackend for FakeBackend {
        fn start_listening(&mut self) -> Result<(), String> {
            self.starts += 1;
            Ok(())
        }

        fn stop_listening(&mut self) -> Result<(), String> {
            self.stops += 1;
            Ok(())
        }
    }

    #[test]
    fn fake_backend_starts_once_and_escape_stops_without_abort() {
        let mut controller = TuiVoiceInputController::new(FakeBackend::default());
        assert_eq!(controller.start(), Ok(true));
        assert_eq!(controller.start(), Ok(false));
        assert_eq!(controller.state(), TuiVoiceInputState::Listening);
        assert_eq!(controller.stop(), Ok(true));
        assert_eq!(controller.stop(), Ok(true));
        assert_eq!(controller.state(), TuiVoiceInputState::Transcribing);
        assert_eq!(controller.backend.starts, 1);
        assert_eq!(controller.backend.stops, 1);
    }

    #[test]
    fn stale_completion_cannot_mutate_a_later_session() {
        let mut controller = TuiVoiceInputController::new(FakeBackend::default());
        assert_eq!(controller.start(), Ok(true));
        let first_generation = controller.generation();
        assert_eq!(controller.stop(), Ok(true));
        assert!(controller.complete(first_generation, "hello".to_owned()));

        assert_eq!(controller.start(), Ok(true));
        let second_generation = controller.generation();
        assert_ne!(first_generation, second_generation);
        assert!(!controller.complete(first_generation, "stale".to_owned()));
        assert_eq!(controller.state(), TuiVoiceInputState::Listening);
    }

    #[test]
    fn failed_transcription_returns_to_idle() {
        let mut controller = TuiVoiceInputController::new(FakeBackend::default());
        assert_eq!(controller.start(), Ok(true));
        let generation = controller.generation();
        assert_eq!(controller.stop(), Ok(true));
        assert!(controller.fail(generation));
        assert_eq!(controller.state(), TuiVoiceInputState::Idle);
    }
}
