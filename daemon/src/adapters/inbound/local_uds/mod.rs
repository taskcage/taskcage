//! Local Protocol v1/v2의 UDS inbound adapter다.

pub(crate) mod codec;
pub(crate) mod mapper;
#[cfg(target_os = "linux")]
pub(crate) mod server;
