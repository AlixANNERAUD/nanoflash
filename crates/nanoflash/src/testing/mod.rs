//! Transcripts of a serial session, to replay against a backend and to record from a real one.

mod record;
mod replay;
mod transcript;

pub use record::{RecordingPort, RecordingProvider, record};
pub use replay::{ReplayPort, ReplayProvider, replay};
pub use transcript::{Entry, Event, HostToken, Transcript, TranscriptError};
