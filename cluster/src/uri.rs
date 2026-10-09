//! Aeron channel URI construction via `AeronUriStringBuilder`.
//!
//! # Performance over convenience
//!
//! Prefer **`CString` / `CStr`** for channels that end up at rusteron/Aeron C.
//! Returning `String` / `&str` when you still need a C string means **extra**
//! work (UTF-8 view + a second NUL conversion). Do **not** do that for
//! performance-sensitive paths.
//!
//! | Form | Cost | Use |
//! |------|------|-----|
//! | [`AERON_IPC_STREAM`] | zero | static IPC (rusteron; do not re-define) |
//! | `channel_cstr` / `udp_endpoint_cstr` | one normalize + one CString | dynamic channels for rusteron |
//! | `String` / `&str` | only when you truly need UTF-8 text and **not** FFI next | rare |
//!
//! `AeronUriStringBuilder::build` produces a temporary `String`; we convert
//! once to `CString` for the public API so callers pass `&CStr` to rusteron
//! without a second `cformat!`.

use std::ffi::{CStr, CString};

use rusteron_client::{AeronCError, AeronUriStringBuilder, cformat};

use crate::ClusterError;

/// IPC channel — re-export of rusteron's zero-cost `c"aeron:ipc"`.
///
/// Prefer this over inventing another `c"aeron:ipc"` / owned `CString`.
pub use rusteron_client::AERON_IPC_STREAM;

fn map_uri(e: AeronCError) -> ClusterError {
    let reason = e.to_string();
    let source: crate::error::AeronErrorSource = e.into();
    ClusterError::ChannelUri { reason, source }
}

/// Parse and normalize a full Aeron channel URI into a `CString` for rusteron.
pub fn channel_cstr(uri: &str) -> Result<CString, ClusterError> {
    let s = {
        let builder: AeronUriStringBuilder = uri.parse().map_err(map_uri)?;
        builder.build(512).map_err(map_uri)?
    };
    Ok(cformat!("{s}"))
}

/// The configured ingress channel pointed at another member's `endpoint`,
/// keeping every other setting (Java `ChannelUri.put(ENDPOINT, …)`):
/// `term-length` and `mtu` fix the publication's message limits, so dropping
/// them would change [`crate::AeronCluster::ingress_max_message_length`] on a
/// REDIRECT or failover. Without a configured UDP channel the result is
/// `aeron:udp?endpoint={endpoint}`.
pub(crate) fn ingress_channel_for_endpoint(template: Option<&CStr>, endpoint: &str) -> Result<CString, ClusterError> {
    let Some(template) = template.filter(|t| t.to_bytes().starts_with(b"aeron:udp")) else {
        return udp_endpoint_cstr(endpoint);
    };
    let builder = AeronUriStringBuilder::default();
    builder.init_on_string(template).map_err(map_uri)?;
    builder.endpoint(endpoint).map_err(map_uri)?;
    let s = builder.build(512).map_err(map_uri)?;
    Ok(cformat!("{s}"))
}

/// Build `aeron:udp?endpoint={endpoint}` as `CString` for rusteron.
///
/// `endpoint` is `host:port` (no `aeron:` prefix).
pub fn udp_endpoint_cstr(endpoint: &str) -> Result<CString, ClusterError> {
    let s = AeronUriStringBuilder::udp(endpoint)
        .and_then(|b| b.build(256))
        .map_err(map_uri)?;
    Ok(cformat!("{s}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rusteron_ipc_is_aeron_ipc() -> Result<(), Box<dyn std::error::Error>> {
        assert_eq!(AERON_IPC_STREAM.to_bytes(), b"aeron:ipc");
        Ok(())
    }

    #[test]
    fn udp_endpoint_builds() -> Result<(), Box<dyn std::error::Error>> {
        let c = udp_endpoint_cstr("localhost:19099")?;
        let s = c.to_str()?;
        assert!(s.starts_with("aeron:udp?"), "{s}");
        assert!(s.contains("endpoint=localhost:19099"), "{s}");
        Ok(())
    }

    #[test]
    fn full_uri_parse() -> Result<(), Box<dyn std::error::Error>> {
        let c = channel_cstr("aeron:udp?endpoint=127.0.0.1:9002")?;
        assert!(c.to_str()?.contains("endpoint=127.0.0.1:9002"));
        Ok(())
    }

    #[test]
    fn ingress_channel_for_endpoint_keeps_the_settings() -> Result<(), Box<dyn std::error::Error>> {
        let template = channel_cstr("aeron:udp?endpoint=a:1|term-length=1m|mtu=4096")?;
        let c = ingress_channel_for_endpoint(Some(&template), "b:2")?;
        let s = c.to_str()?;
        let mut params: Vec<&str> = s.strip_prefix("aeron:udp?").ok_or(s)?.split('|').collect();
        params.sort_unstable();
        assert_eq!(params, ["endpoint=b:2", "mtu=4096", "term-length=1m"], "{s}");
        Ok(())
    }

    #[test]
    fn ingress_channel_for_endpoint_without_a_udp_template_is_the_bare_endpoint()
    -> Result<(), Box<dyn std::error::Error>> {
        let ipc = channel_cstr("aeron:ipc?term-length=1m")?;
        for template in [None, Some(ipc.as_c_str())] {
            let c = ingress_channel_for_endpoint(template, "b:2")?;
            assert_eq!(c.to_str()?, udp_endpoint_cstr("b:2")?.to_str()?);
        }
        Ok(())
    }
}
