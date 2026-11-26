//! Stable error type and error codes for the MOTU CueMix control path.
//!
//! Every fallible public API returns [CueMixError]; [CueMixError::code] is the
//! stable machine-readable code (never change an existing string, add new ones
//! instead). The daemon must branch on the code, not on Display text.

use std::fmt;

/// Stable error codes. Treat these strings as a wire contract.
pub mod codes {
    /// Local socket I/O failure (connect / read / write).
    pub const IO: &str = "E_IO";
    /// A bounded wait expired (connect, handshake, sync, readback).
    pub const TIMEOUT: &str = "E_TIMEOUT";
    /// WebSocket HTTP upgrade failed (status, headers, accept key).
    pub const HANDSHAKE: &str = "E_HANDSHAKE";
    /// Malformed frame or datastore message.
    pub const PROTOCOL: &str = "E_PROTOCOL";
    /// Peer closed the connection.
    pub const CLOSED: &str = "E_CLOSED";
    /// The device requires a control-network password but none was configured.
    pub const AUTH_REQUIRED: &str = "E_AUTH_REQUIRED";
    /// Authentication could not be completed (order violation, no data after auth).
    pub const AUTH_FAILED: &str = "E_AUTH_FAILED";
    /// No usable device: platform/card missing or control network unreachable.
    pub const NOT_DETECTED: &str = "E_NOT_DETECTED";
    /// A value is outside the accepted range or not finite.
    pub const RANGE: &str = "E_RANGE";
    /// The device did not echo the written value within the readback deadline.
    pub const READBACK_TIMEOUT: &str = "E_READBACK_TIMEOUT";
    /// The device accepted the write but streamed back a different value.
    pub const READBACK_MISMATCH: &str = "E_READBACK_MISMATCH";
    /// Operation not supported by this client (e.g. writing a read-only type).
    pub const UNSUPPORTED: &str = "E_UNSUPPORTED";
}

/// One failure of the CueMix control path.
#[derive(Debug)]
pub enum CueMixError {
    /// Socket-level I/O failure; `context` names the operation.
    Io {
        /// Operation that failed (stable label, e.g. "connect").
        context: &'static str,
        /// Underlying I/O error.
        source: std::io::Error,
    },
    /// A bounded wait expired.
    Timeout {
        /// Operation that timed out.
        context: &'static str,
    },
    /// WebSocket upgrade was rejected or malformed.
    Handshake {
        /// Human-readable reason.
        reason: String,
    },
    /// Malformed frame or datastore message.
    Protocol {
        /// Human-readable reason.
        reason: String,
    },
    /// The peer closed the connection.
    Closed,
    /// The device requires a password and none was configured.
    AuthRequired,
    /// Authentication could not complete.
    AuthFailed {
        /// Human-readable reason.
        reason: String,
    },
    /// No usable device was detected.
    NotDetected {
        /// Stable reason (see detect module REASON_* constants).
        reason: &'static str,
    },
    /// A value is out of range / not finite.
    Range {
        /// Field or argument name.
        field: &'static str,
        /// Human-readable reason.
        reason: String,
    },
    /// The device did not stream the written value back in time.
    ReadbackTimeout {
        /// Datastore key.
        key: &'static str,
        /// Datastore index.
        index: u16,
    },
    /// The device streamed a different value than the one written.
    ReadbackMismatch {
        /// Datastore key.
        key: &'static str,
        /// Datastore index.
        index: u16,
        /// Value we wrote.
        expected: String,
        /// Value the device reported.
        actual: String,
    },
    /// The requested operation is not supported.
    Unsupported {
        /// What was requested.
        what: &'static str,
    },
}

impl CueMixError {
    /// Build an I/O error with a stable operation label.
    pub fn io(context: &'static str, source: std::io::Error) -> Self {
        Self::Io { context, source }
    }

    /// Build a timeout error with a stable operation label.
    pub fn timeout(context: &'static str) -> Self {
        Self::Timeout { context }
    }

    /// Stable machine-readable code; see [codes].
    pub fn code(&self) -> &'static str {
        match self {
            Self::Io { .. } => codes::IO,
            Self::Timeout { .. } => codes::TIMEOUT,
            Self::Handshake { .. } => codes::HANDSHAKE,
            Self::Protocol { .. } => codes::PROTOCOL,
            Self::Closed => codes::CLOSED,
            Self::AuthRequired => codes::AUTH_REQUIRED,
            Self::AuthFailed { .. } => codes::AUTH_FAILED,
            Self::NotDetected { .. } => codes::NOT_DETECTED,
            Self::Range { .. } => codes::RANGE,
            Self::ReadbackTimeout { .. } => codes::READBACK_TIMEOUT,
            Self::ReadbackMismatch { .. } => codes::READBACK_MISMATCH,
            Self::Unsupported { .. } => codes::UNSUPPORTED,
        }
    }

    /// True when a plain retry (after the device settles) can plausibly succeed.
    pub fn is_retryable(&self) -> bool {
        matches!(
            self,
            Self::Io { .. }
                | Self::Timeout { .. }
                | Self::Closed
                | Self::ReadbackTimeout { .. }
                | Self::ReadbackMismatch { .. }
        )
    }
}

impl fmt::Display for CueMixError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io { context, source } => write!(f, "{}: {}", context, source),
            Self::Timeout { context } => write!(f, "{}: timed out", context),
            Self::Handshake { reason } => write!(f, "websocket handshake: {}", reason),
            Self::Protocol { reason } => write!(f, "protocol: {}", reason),
            Self::Closed => write!(f, "connection closed by peer"),
            Self::AuthRequired => write!(
                f,
                "device requires a control password but none was configured"
            ),
            Self::AuthFailed { reason } => write!(f, "authentication failed: {}", reason),
            Self::NotDetected { reason } => write!(f, "device not detected: {}", reason),
            Self::Range { field, reason } => write!(f, "{}: {}", field, reason),
            Self::ReadbackTimeout { key, index } => {
                write!(
                    f,
                    "readback timeout: {}[{}] not echoed by device",
                    key, index
                )
            }
            Self::ReadbackMismatch {
                key,
                index,
                expected,
                actual,
            } => write!(
                f,
                "readback mismatch: {}[{}] wrote {} but device reports {}",
                key, index, expected, actual
            ),
            Self::Unsupported { what } => write!(f, "unsupported: {}", what),
        }
    }
}

impl std::error::Error for CueMixError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            _ => None,
        }
    }
}
