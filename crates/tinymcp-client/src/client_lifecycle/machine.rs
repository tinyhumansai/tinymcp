//! The per-directory open/close state machine.

use crate::{McpClientConfig, OpenConfiguredResponse, TransitionStatus};

/// A directory object the module reported as open, with the configuration it
/// was opened with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenObject {
    /// The configuration the module is serving.
    pub config: McpClientConfig,
    /// The module-assigned child object path.
    pub object_path: String,
}

#[derive(Debug, Clone)]
enum State {
    Closed,
    StartingOpen {
        config: McpClientConfig,
        previous: Option<OpenObject>,
    },
    Opening {
        config: McpClientConfig,
        previous: Option<OpenObject>,
        receipt: OpenConfiguredResponse,
    },
    Open(OpenObject),
    StartingClose(OpenObject),
    Closing {
        object: OpenObject,
        transition_id: String,
    },
}

/// The bus call a driver must make next.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Step {
    /// The directory is open with the requested configuration.
    Ready(OpenObject),
    /// Call `OpenConfigured` with this configuration, then
    /// [`ConfiguredLifecycle::open_replied`].
    CallOpen(McpClientConfig),
    /// Call `TransitionStatus` for this id, then
    /// [`ConfiguredLifecycle::open_polled`].
    PollOpen(String),
    /// Call `Close`, then [`ConfiguredLifecycle::close_replied`].
    CallClose,
    /// Call `TransitionStatus` for this id, then
    /// [`ConfiguredLifecycle::close_polled`].
    PollClose(String),
    /// The directory is closed and the module acknowledged it.
    Closed,
}

/// Why a driver must abandon the current attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Fault {
    /// A transition ended twice in a row without completing.
    Transport,
    /// The module reported a status this contract does not define.
    Module,
}

/// What a polled transition status means for the driver.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Poll {
    /// Ask the machine for its next [`Step`].
    Continue,
    /// Same as `Continue`; the transition finished and the state advanced.
    Done,
    /// Give up on this attempt with the fault. State is kept for a retry.
    Fault(Fault),
}

/// One directory's serialized lifecycle. Not thread-safe by itself: the host
/// holds it behind the lock that serializes that directory's calls.
#[derive(Debug, Clone)]
pub struct ConfiguredLifecycle {
    state: State,
    retried_terminal: bool,
}

impl Default for ConfiguredLifecycle {
    fn default() -> Self {
        Self::new()
    }
}

impl ConfiguredLifecycle {
    /// A lifecycle with nothing open.
    #[must_use]
    pub fn new() -> Self {
        Self {
            state: State::Closed,
            retried_terminal: false,
        }
    }

    /// Whether the module has acknowledged that nothing is open.
    #[must_use]
    pub fn is_closed(&self) -> bool {
        matches!(self.state, State::Closed)
    }

    /// The object currently open and settled, if any.
    #[must_use]
    pub fn open_object(&self) -> Option<&OpenObject> {
        match &self.state {
            State::Open(object) => Some(object),
            _ => None,
        }
    }

    /// The next step toward having `requested` open. A different open
    /// configuration, or a close still in flight, is resolved first.
    pub fn open_step(&mut self, requested: &McpClientConfig) -> Step {
        match self.state.clone() {
            State::Open(object) if object.config == *requested => Step::Ready(object),
            State::Open(object) => {
                self.state = State::StartingOpen {
                    config: requested.clone(),
                    previous: Some(object),
                };
                Step::CallOpen(requested.clone())
            }
            State::Closed => {
                self.state = State::StartingOpen {
                    config: requested.clone(),
                    previous: None,
                };
                Step::CallOpen(requested.clone())
            }
            State::StartingOpen { config, .. } => Step::CallOpen(config),
            State::Opening { receipt, .. } => Step::PollOpen(receipt.transition_id),
            State::StartingClose(_) => Step::CallClose,
            State::Closing { transition_id, .. } => Step::PollClose(transition_id),
        }
    }

    /// The next step toward a confirmed close. An open still in flight is
    /// finished first, then closed.
    pub fn close_step(&mut self) -> Step {
        match self.state.clone() {
            State::Closed => Step::Closed,
            State::Open(object) => {
                self.state = State::StartingClose(object);
                Step::CallClose
            }
            State::StartingOpen { config, .. } => Step::CallOpen(config),
            State::Opening { receipt, .. } => Step::PollOpen(receipt.transition_id),
            State::StartingClose(_) => Step::CallClose,
            State::Closing { transition_id, .. } => Step::PollClose(transition_id),
        }
    }

    /// Records the reply to `OpenConfigured`. `Err` restores the previous
    /// object (or closed), because nothing was accepted.
    pub fn open_replied(&mut self, reply: Result<OpenConfiguredResponse, ()>) {
        let State::StartingOpen { config, previous } = self.state.clone() else {
            return;
        };
        if let Ok(receipt) = reply {
            self.state = State::Opening {
                config,
                previous,
                receipt,
            };
        } else {
            self.retried_terminal = false;
            self.state = previous.map_or(State::Closed, State::Open);
        }
    }

    /// Records a polled status for the open receipt.
    pub fn open_polled(&mut self, status: TransitionStatus) -> Poll {
        let State::Opening {
            config,
            previous,
            receipt,
        } = self.state.clone()
        else {
            return Poll::Continue;
        };
        match status {
            TransitionStatus::Pending => Poll::Continue,
            TransitionStatus::Complete => {
                self.retried_terminal = false;
                self.state = State::Open(OpenObject {
                    config,
                    object_path: receipt.object_path,
                });
                Poll::Done
            }
            TransitionStatus::Failed | TransitionStatus::Unknown => {
                if self.retried_terminal {
                    self.retried_terminal = false;
                    return Poll::Fault(Fault::Transport);
                }
                self.retried_terminal = true;
                self.state = State::StartingOpen { config, previous };
                Poll::Continue
            }
            #[allow(unreachable_patterns)]
            _ => Poll::Fault(Fault::Module),
        }
    }

    /// Records the reply to `Close`. `Ok(None)` means cleanup already ended; an
    /// error keeps the state so the next call retries the same close.
    pub fn close_replied(&mut self, reply: Result<Option<String>, ()>) {
        let State::StartingClose(object) = self.state.clone() else {
            return;
        };
        match reply {
            Ok(Some(transition_id)) => {
                self.state = State::Closing {
                    object,
                    transition_id,
                };
            }
            Ok(None) => {
                self.retried_terminal = false;
                self.state = State::Closed;
            }
            Err(()) => {}
        }
    }

    /// Records a polled status for the close receipt.
    pub fn close_polled(&mut self, status: TransitionStatus) -> Poll {
        let State::Closing { object, .. } = self.state.clone() else {
            return Poll::Continue;
        };
        match status {
            TransitionStatus::Pending => Poll::Continue,
            TransitionStatus::Complete => {
                self.retried_terminal = false;
                self.state = State::Closed;
                Poll::Done
            }
            TransitionStatus::Failed | TransitionStatus::Unknown => {
                if self.retried_terminal {
                    self.retried_terminal = false;
                    return Poll::Fault(Fault::Transport);
                }
                self.retried_terminal = true;
                self.state = State::StartingClose(object);
                Poll::Continue
            }
            #[allow(unreachable_patterns)]
            _ => Poll::Fault(Fault::Module),
        }
    }
}
