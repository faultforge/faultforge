//! Error types for the management API client.

/// Errors that can occur when calling the management API.
///
/// Each variant maps to a distinct, user-facing diagnostic. Transport-level
/// failures are classified at the API boundary (see `api::classify_transport`)
/// so the operator sees "could not connect" / "timed out" rather than reqwest's
/// verbose internal message.
#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    /// The master refused the connection or could not be reached (connection
    /// refused, DNS failure). `url` is the base URL that was targeted.
    #[error("could not connect to master at {url}")]
    Connect {
        /// The configured master base URL that could not be reached.
        url: String,
    },

    /// The request exceeded the configured timeout (connect or total).
    #[error("request to master timed out")]
    Timeout,

    /// Any other transport-level failure not covered by [`Self::Connect`] or
    /// [`Self::Timeout`] (e.g. a TLS handshake error or a broken stream).
    #[error("transport error: {0}")]
    Transport(String),

    /// The requested resource was not found (HTTP 404).
    #[error("not found")]
    NotFound,

    /// The request conflicts with the resource's current state (HTTP 409),
    /// e.g. halting a concluded experiment or clearing taint on a
    /// disconnected host.
    #[error("conflict: {0}")]
    Conflict(String),

    /// The master rejected the experiment at VALIDATE (HTTP 422); every
    /// failing check is listed.
    #[error("experiment rejected: {}", .0.join("; "))]
    Validation(Vec<String>),

    /// The master returned an unexpected non-2xx status (e.g. 500). `body` is
    /// retained for diagnostics but omitted from the short `Display` message.
    #[error("master returned HTTP {status}")]
    Status {
        /// The HTTP status code returned by the master.
        status: u16,
        /// The response body, kept for logging; not shown in `Display`.
        body: String,
    },

    /// The response body could not be decoded as the expected JSON shape.
    #[error("could not decode master response: {0}")]
    Decode(String),

    /// The configured `--master-url` is not a valid base URL.
    #[error("invalid master URL: {0}")]
    InvalidUrl(String),
}
