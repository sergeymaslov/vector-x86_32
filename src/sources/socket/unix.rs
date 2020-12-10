use crate::{
    event::{self, Event},
    internal_events::UnixSocketEventReceived,
    shutdown::ShutdownSignal,
    sources::{util::build_unix_source, Source},
};
use bytes::Bytes;
use futures01::sync::mpsc;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use tokio01::codec::LinesCodec;

#[derive(Deserialize, Serialize, Debug, Clone)]
#[serde(deny_unknown_fields)]
pub struct UnixConfig {
    pub path: PathBuf,
    #[serde(default = "default_max_length")]
    pub max_length: usize,
    pub host_key: Option<String>,
}

fn default_max_length() -> usize {
    bytesize::kib(100u64) as usize
}

impl UnixConfig {
    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            max_length: default_max_length(),
            host_key: None,
        }
    }
}

/**
* Function to pass to build_unix_source, specific to the basic unix source.
* Takes a single line of a received message and builds an Event object.
**/
fn build_event(host_key: &str, received_from: Option<Bytes>, line: &str) -> Option<Event> {
    let byte_size = line.len();
    let mut event = Event::from(line);
    event
        .as_mut_log()
        .insert(event::log_schema().source_type_key(), "socket");
    if let Some(host) = received_from {
        event.as_mut_log().insert(host_key, host);
    }
    emit!(UnixSocketEventReceived { byte_size });
    Some(event)
}

pub fn unix(
    path: PathBuf,
    max_length: usize,
    host_key: String,
    shutdown: ShutdownSignal,
    out: mpsc::Sender<Event>,
) -> Source {
    build_unix_source(
        path,
        LinesCodec::new_with_max_length(max_length),
        host_key,
        shutdown,
        out,
        build_event,
    )
}
