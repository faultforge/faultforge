//! HTTP client for the master's management API (registry reads plus the
//! experiment/taint write surface).

pub mod error;
pub mod model;

pub use error::ApiError;
pub use model::{Agent, Experiment, ExperimentSummary};

use std::time::Duration;

use reqwest::{Client as HttpClient, StatusCode, Url};

/// Maximum time to establish a TCP/TLS connection to the master.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
/// Maximum total time for a single request (connect + send + receive).
///
/// Bounds how long a one-shot command or a TUI fetch waits on a master that
/// accepts the connection but never replies, so neither blocks indefinitely.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

/// Client for the master management API.
///
/// Holds an async `reqwest` HTTP client and the parsed base URL.
/// Construct once and reuse across calls. `Clone` is cheap: the inner HTTP
/// client uses an `Arc`-backed connection pool.
#[derive(Clone)]
pub struct Client {
    base_url: Url,
    http: HttpClient,
}

impl Client {
    /// Create a new client targeting `base_url` (e.g. `http://localhost:8069`).
    ///
    /// The base URL is parsed and validated up front so a malformed
    /// `--master-url` fails fast with a clear message rather than a late
    /// transport error. Connect and request timeouts are applied so a hung
    /// master cannot block the caller indefinitely.
    ///
    /// # Errors
    ///
    /// - [`ApiError::InvalidUrl`] — `base_url` is not a valid URL.
    /// - [`ApiError::Transport`] — the underlying HTTP client could not be built.
    pub fn new(base_url: impl AsRef<str>) -> Result<Self, ApiError> {
        let base_url = Url::parse(base_url.as_ref())
            .map_err(|e| ApiError::InvalidUrl(format!("{}: {e}", base_url.as_ref())))?;

        let http = HttpClient::builder()
            .connect_timeout(CONNECT_TIMEOUT)
            .timeout(REQUEST_TIMEOUT)
            .user_agent(concat!("faultforge-cli/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|e| ApiError::Transport(e.to_string()))?;

        Ok(Self { base_url, http })
    }

    /// Fetch all registered agents (`GET /agents`).
    ///
    /// Returns an empty `Vec` when no agents are registered.
    ///
    /// # Errors
    ///
    /// - [`ApiError::Connect`] / [`ApiError::Timeout`] / [`ApiError::Transport`] —
    ///   the master could not be reached.
    /// - [`ApiError::Status`] — an unexpected non-2xx response.
    /// - [`ApiError::Decode`] — the response body was not valid JSON.
    /// - [`ApiError::InvalidUrl`] — the base URL cannot form a request path.
    pub async fn list_agents(&self) -> Result<Vec<Agent>, ApiError> {
        let url = self.endpoint(&["agents"])?;
        let response = self
            .http
            .get(url.clone())
            .send()
            .await
            .map_err(|e| classify_transport(&e, &self.base_url))?;

        if !response.status().is_success() {
            return Err(status_error(response).await);
        }

        response
            .json::<Vec<Agent>>()
            .await
            .map_err(|e| ApiError::Decode(e.to_string()))
    }

    /// Fetch a single agent by hostname (`GET /agents/{hostname}`).
    ///
    /// `hostname` is percent-encoded as a single path segment, so values
    /// containing spaces or slashes form a well-defined request URL.
    ///
    /// # Errors
    ///
    /// - [`ApiError::Connect`] / [`ApiError::Timeout`] / [`ApiError::Transport`] —
    ///   the master could not be reached.
    /// - [`ApiError::NotFound`] — no agent is registered for `hostname`.
    /// - [`ApiError::Status`] — an unexpected non-2xx response.
    /// - [`ApiError::Decode`] — the response body was not valid JSON.
    /// - [`ApiError::InvalidUrl`] — the base URL cannot form a request path.
    pub async fn get_agent(&self, hostname: &str) -> Result<Agent, ApiError> {
        let url = self.endpoint(&["agents", hostname])?;
        let response = self
            .http
            .get(url.clone())
            .send()
            .await
            .map_err(|e| classify_transport(&e, &self.base_url))?;

        if response.status() == StatusCode::NOT_FOUND {
            return Err(ApiError::NotFound);
        }

        if !response.status().is_success() {
            return Err(status_error(response).await);
        }

        response
            .json::<Agent>()
            .await
            .map_err(|e| ApiError::Decode(e.to_string()))
    }

    /// Submit an experiment definition (`POST /experiments`).
    ///
    /// `definition` is the already-parsed JSON body (the CLI reads it from a
    /// YAML/JSON file); the master validates it.
    ///
    /// # Errors
    ///
    /// - [`ApiError::Validation`] — the master rejected the definition (HTTP
    ///   422); every failing check is listed.
    /// - Transport/status/decode errors as for the other calls.
    pub async fn run_experiment(
        &self,
        definition: &serde_json::Value,
    ) -> Result<Experiment, ApiError> {
        let url = self.endpoint(&["experiments"])?;
        let response = self
            .http
            .post(url)
            .json(definition)
            .send()
            .await
            .map_err(|e| classify_transport(&e, &self.base_url))?;

        if response.status() == StatusCode::UNPROCESSABLE_ENTITY {
            return Err(validation_error(response).await);
        }
        if !response.status().is_success() {
            return Err(status_error(response).await);
        }
        response
            .json::<Experiment>()
            .await
            .map_err(|e| ApiError::Decode(e.to_string()))
    }

    /// Fetch all experiments (`GET /experiments`). Empty when the master holds
    /// none (including right after a master restart).
    ///
    /// # Errors
    ///
    /// Transport/status/decode errors as for [`Self::list_agents`].
    pub async fn list_experiments(&self) -> Result<Vec<ExperimentSummary>, ApiError> {
        let url = self.endpoint(&["experiments"])?;
        let response = self
            .http
            .get(url)
            .send()
            .await
            .map_err(|e| classify_transport(&e, &self.base_url))?;
        if !response.status().is_success() {
            return Err(status_error(response).await);
        }
        response
            .json::<Vec<ExperimentSummary>>()
            .await
            .map_err(|e| ApiError::Decode(e.to_string()))
    }

    /// Fetch one experiment (`GET /experiments/{id}`).
    ///
    /// # Errors
    ///
    /// - [`ApiError::NotFound`] — the master holds no such experiment.
    /// - Transport/status/decode errors as for the other calls.
    pub async fn get_experiment(&self, id: &str) -> Result<Experiment, ApiError> {
        let url = self.endpoint(&["experiments", id])?;
        let response = self
            .http
            .get(url)
            .send()
            .await
            .map_err(|e| classify_transport(&e, &self.base_url))?;
        if response.status() == StatusCode::NOT_FOUND {
            return Err(ApiError::NotFound);
        }
        if !response.status().is_success() {
            return Err(status_error(response).await);
        }
        response
            .json::<Experiment>()
            .await
            .map_err(|e| ApiError::Decode(e.to_string()))
    }

    /// Halt an experiment (`POST /experiments/{id}/halt`).
    ///
    /// # Errors
    ///
    /// - [`ApiError::NotFound`] — no such experiment.
    /// - [`ApiError::Conflict`] — the experiment is already terminal.
    /// - Transport/status errors as for the other calls.
    pub async fn halt_experiment(&self, id: &str) -> Result<(), ApiError> {
        let url = self.endpoint(&["experiments", id, "halt"])?;
        let response = self
            .http
            .post(url)
            .send()
            .await
            .map_err(|e| classify_transport(&e, &self.base_url))?;
        match response.status() {
            StatusCode::NOT_FOUND => Err(ApiError::NotFound),
            StatusCode::CONFLICT => Err(ApiError::Conflict(
                "experiment is already terminal".to_string(),
            )),
            status if status.is_success() => Ok(()),
            _ => Err(status_error(response).await),
        }
    }

    /// Clear a host's taint (`POST /agents/{hostname}/clear-taint`).
    ///
    /// A `202` means the command was relayed; the effect is confirmed by the
    /// agent's next `TaintStatus` (visible via `agents show`).
    ///
    /// # Errors
    ///
    /// - [`ApiError::NotFound`] — the hostname is not registered.
    /// - [`ApiError::Conflict`] — the host has no live session.
    /// - Transport/status errors as for the other calls.
    pub async fn clear_taint(&self, hostname: &str) -> Result<(), ApiError> {
        let url = self.endpoint(&["agents", hostname, "clear-taint"])?;
        let response = self
            .http
            .post(url)
            .send()
            .await
            .map_err(|e| classify_transport(&e, &self.base_url))?;
        match response.status() {
            StatusCode::NOT_FOUND => Err(ApiError::NotFound),
            StatusCode::CONFLICT => Err(ApiError::Conflict(
                "host has no live session; the agent must be connected to clear its taint"
                    .to_string(),
            )),
            status if status.is_success() => Ok(()),
            _ => Err(status_error(response).await),
        }
    }

    /// Build a request URL by appending percent-encoded `segments` onto the base.
    ///
    /// Each segment is encoded as a single path segment (handling spaces and
    /// slashes), and a trailing empty segment from a base like
    /// `http://host:8069/` is dropped first so the result never doubles a slash.
    fn endpoint(&self, segments: &[&str]) -> Result<Url, ApiError> {
        let mut url = self.base_url.clone();
        url.path_segments_mut()
            .map_err(|()| ApiError::InvalidUrl(self.base_url.to_string()))?
            .pop_if_empty()
            .extend(segments);
        Ok(url)
    }
}

/// Classify a reqwest send error into a user-facing [`ApiError`].
///
/// Distinguishes connection refusal and timeouts — the cases an operator can
/// act on — from any other transport failure.
fn classify_transport(e: &reqwest::Error, base_url: &Url) -> ApiError {
    if e.is_timeout() {
        ApiError::Timeout
    } else if e.is_connect() {
        ApiError::Connect {
            url: base_url.to_string(),
        }
    } else {
        ApiError::Transport(e.to_string())
    }
}

/// Build an [`ApiError::Status`] from a non-2xx response, consuming it.
async fn status_error(response: reqwest::Response) -> ApiError {
    let status = response.status().as_u16();
    let body = response.text().await.unwrap_or_default();
    ApiError::Status { status, body }
}

/// Build an [`ApiError::Validation`] from a 422 body (`{ "errors": [...] }`),
/// consuming the response. An unparseable body degrades to its raw text so the
/// operator still sees why the master said no.
async fn validation_error(response: reqwest::Response) -> ApiError {
    #[derive(serde::Deserialize)]
    struct Errors {
        errors: Vec<String>,
    }
    let body = response.text().await.unwrap_or_default();
    match serde_json::from_str::<Errors>(&body) {
        Ok(parsed) => ApiError::Validation(parsed.errors),
        Err(_) => ApiError::Validation(vec![body]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn client() -> Client {
        Client::new("http://localhost:8069").expect("valid base URL")
    }

    #[test]
    fn decode_error_carries_message() {
        let err = ApiError::Decode("unexpected field".to_string());
        assert!(matches!(err, ApiError::Decode(ref msg) if msg.contains("unexpected")));
    }

    #[test]
    fn not_found_variant_is_distinct_from_status() {
        let nf = ApiError::NotFound;
        let st = ApiError::Status {
            status: 500,
            body: String::new(),
        };
        assert!(matches!(nf, ApiError::NotFound));
        assert!(matches!(st, ApiError::Status { status: 500, .. }));
    }

    #[test]
    fn agent_deserializes_from_wire_json() {
        use std::time::{Duration, UNIX_EPOCH};
        let json = r#"{"hostname":"web-01","name":"web-01","last_seen_unix_ms":1000}"#;
        let agent: Agent = serde_json::from_str(json).expect("valid agent JSON");
        assert_eq!(agent.hostname, "web-01");
        assert_eq!(agent.last_seen, UNIX_EPOCH + Duration::from_secs(1));
    }

    #[test]
    fn agent_deserialize_rejects_bad_json() {
        let result: Result<Agent, _> = serde_json::from_str("{not json}");
        assert!(result.is_err());
    }

    #[test]
    fn invalid_base_url_is_rejected() {
        assert!(matches!(
            Client::new("not a url"),
            Err(ApiError::InvalidUrl(_))
        ));
    }

    #[test]
    fn list_endpoint_has_no_doubled_slash_without_trailing_slash() {
        let c = client();
        assert_eq!(
            c.endpoint(&["agents"]).unwrap().as_str(),
            "http://localhost:8069/agents"
        );
    }

    #[test]
    fn list_endpoint_has_no_doubled_slash_with_trailing_slash() {
        let c = Client::new("http://localhost:8069/").expect("valid base URL");
        assert_eq!(
            c.endpoint(&["agents"]).unwrap().as_str(),
            "http://localhost:8069/agents"
        );
    }

    #[test]
    fn hostname_segment_is_percent_encoded() {
        let c = client();
        // A space and a slash must be encoded so they stay within one path segment.
        let url = c.endpoint(&["agents", "web 01"]).unwrap();
        assert_eq!(url.as_str(), "http://localhost:8069/agents/web%2001");

        let url = c.endpoint(&["agents", "a/b"]).unwrap();
        assert_eq!(url.as_str(), "http://localhost:8069/agents/a%2Fb");
    }

    #[test]
    fn endpoint_preserves_base_path() {
        let c = Client::new("http://localhost:8069/api/").expect("valid base URL");
        assert_eq!(
            c.endpoint(&["agents", "web-01"]).unwrap().as_str(),
            "http://localhost:8069/api/agents/web-01"
        );
    }

    // ===== HTTP-mock tests (axum stand-in for the master's management API) =====

    mod mock {
        use super::super::*;
        use axum::http::StatusCode as AxStatus;
        use axum::routing::{get, post};
        use axum::{Json, Router};
        use serde_json::json;

        /// Serve `router` on an ephemeral port; return a client aimed at it.
        async fn serve(router: Router) -> Client {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            tokio::spawn(async move {
                axum::serve(listener, router).await.unwrap();
            });
            Client::new(format!("http://{addr}")).unwrap()
        }

        fn experiment_body() -> serde_json::Value {
            json!({
                "id": "exp-1000-1", "name": "it", "state": "RUNNING", "outcome": null,
                "grace_secs": 10, "started_unix_ms": 1000, "deadline_unix_ms": 46000,
                "cause": null,
                "instances": [{"instance_id": "exp-1000-1:web-01:0", "hostname": "web-01",
                               "action_index": 0, "state": "PENDING", "reason": "",
                               "updated_unix_ms": 1000}]
            })
        }

        #[tokio::test]
        async fn run_experiment_returns_the_created_record() {
            let router = Router::new().route(
                "/experiments",
                post(|| async { (AxStatus::CREATED, Json(experiment_body())) }),
            );
            let client = serve(router).await;
            let experiment = client.run_experiment(&json!({"name": "it"})).await.unwrap();
            assert_eq!(experiment.id, "exp-1000-1");
            assert_eq!(experiment.instances[0].state, "PENDING");
            assert!(!experiment.is_terminal());
        }

        #[tokio::test]
        async fn run_experiment_maps_422_to_validation_with_all_reasons() {
            let router = Router::new().route(
                "/experiments",
                post(|| async {
                    (
                        AxStatus::UNPROCESSABLE_ENTITY,
                        Json(json!({"errors": ["unknown plugin ghost@1", "host 'x' is TAINTED"]})),
                    )
                }),
            );
            let client = serve(router).await;
            let err = client.run_experiment(&json!({})).await.unwrap_err();
            match err {
                ApiError::Validation(errors) => {
                    assert_eq!(errors.len(), 2);
                    assert!(errors[0].contains("unknown plugin"));
                }
                other => panic!("expected Validation, got {other:?}"),
            }
        }

        #[tokio::test]
        async fn halt_maps_202_404_409() {
            let router = Router::new()
                .route(
                    "/experiments/exp-ok/halt",
                    post(|| async { AxStatus::ACCEPTED }),
                )
                .route(
                    "/experiments/exp-done/halt",
                    post(|| async { AxStatus::CONFLICT }),
                );
            let client = serve(router).await;
            client.halt_experiment("exp-ok").await.unwrap();
            assert!(matches!(
                client.halt_experiment("exp-done").await.unwrap_err(),
                ApiError::Conflict(_)
            ));
            assert!(matches!(
                client.halt_experiment("exp-missing").await.unwrap_err(),
                ApiError::NotFound
            ));
        }

        #[tokio::test]
        async fn clear_taint_maps_202_404_409() {
            let router = Router::new()
                .route(
                    "/agents/web-01/clear-taint",
                    post(|| async { AxStatus::ACCEPTED }),
                )
                .route(
                    "/agents/gone-01/clear-taint",
                    post(|| async { AxStatus::CONFLICT }),
                );
            let client = serve(router).await;
            client.clear_taint("web-01").await.unwrap();
            assert!(matches!(
                client.clear_taint("gone-01").await.unwrap_err(),
                ApiError::Conflict(_)
            ));
            assert!(matches!(
                client.clear_taint("nope-01").await.unwrap_err(),
                ApiError::NotFound
            ));
        }

        #[tokio::test]
        async fn experiments_list_get_and_missing() {
            let router = Router::new()
                .route(
                    "/experiments",
                    get(|| async {
                        Json(json!([{"id": "exp-1", "name": "it", "state": "COMPLETED",
                                     "outcome": "COMPLETED", "started_unix_ms": 1,
                                     "instances": 2}]))
                    }),
                )
                .route(
                    "/experiments/exp-1",
                    get(|| async { Json(experiment_body()) }),
                );
            let client = serve(router).await;
            let list = client.list_experiments().await.unwrap();
            assert_eq!(list.len(), 1);
            assert_eq!(list[0].outcome.as_deref(), Some("COMPLETED"));
            let experiment = client.get_experiment("exp-1").await.unwrap();
            assert_eq!(experiment.id, "exp-1000-1");
            assert!(matches!(
                client.get_experiment("exp-2").await.unwrap_err(),
                ApiError::NotFound
            ));
        }

        #[tokio::test]
        async fn agents_carry_the_taint_flag() {
            let router = Router::new().route(
                "/agents",
                get(|| async {
                    Json(json!([{"hostname": "web-01", "name": "web-01",
                                 "last_seen_unix_ms": 1000, "tainted": true}]))
                }),
            );
            let client = serve(router).await;
            let agents = client.list_agents().await.unwrap();
            assert!(agents[0].tainted);
        }
    }
}
