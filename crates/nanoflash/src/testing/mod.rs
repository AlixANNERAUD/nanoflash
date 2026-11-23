//! Transcripts of a serial session, to replay against a backend and to record from a real one.

mod record;
mod replay;
mod transcript;

pub use record::{RecordingPort, RecordingProvider, record};
pub use replay::{ReplayPort, ReplayProvider, replay};
pub use transcript::{Entry, Event, HostToken, Transcript, TranscriptError};

/// Serves the files of a map as images, by path as written in the manifest.
#[cfg(feature = "flash")]
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MemorySource {
    /// The bytes of each file.
    pub files: alloc::collections::BTreeMap<alloc::string::String, alloc::vec::Vec<u8>>,
}

#[cfg(feature = "flash")]
impl crate::flash::ImageSource for MemorySource {
    async fn read(&mut self, path: &str) -> Result<alloc::vec::Vec<u8>, alloc::string::String> {
        self.files
            .get(path)
            .cloned()
            .ok_or_else(|| alloc::string::String::from("no such file"))
    }
}
