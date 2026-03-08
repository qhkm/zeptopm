//! HTTP server — REST API for agent management.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::Router;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Json;
use axum::routing::{get, post};
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;
use tracing::info;

use crate::agent::{AgentHandle, AgentState, AgentStatus, LogEntry};

/// Commands sent from HTTP handlers to the daemon loop.
#[derive(Debug)]
pub enum DaemonCommand {
    /// Start an agent by name (must exist in config).
    Start {
        name: String,
        reply: tokio::sync::oneshot::Sender<Result<String, String>>,
    },
    /// Restart an agent by name (stop + start).
    Restart {
        name: String,
        reply: tokio::sync::oneshot::Sender<Result<String, String>>,
    },
    /// Submit a multi-step orchestrated run.
    SubmitRun {
        task: String,
        reply: tokio::sync::oneshot::Sender<Result<String, String>>,
    },
    /// Get run status.
    GetRunStatus {
        run_id: String,
        reply: tokio::sync::oneshot::Sender<Result<serde_json::Value, String>>,
    },
    /// List all runs.
    ListRuns {
        reply: tokio::sync::oneshot::Sender<Result<serde_json::Value, String>>,
    },
    /// Get the final artifact content for a completed run.
    GetRunResult {
        run_id: String,
        reply: tokio::sync::oneshot::Sender<Result<serde_json::Value, String>>,
    },
    /// Cancel a running run.
    CancelRun {
        run_id: String,
        reply: tokio::sync::oneshot::Sender<Result<String, String>>,
    },
    /// Get daemon metrics.
    GetMetrics {
        reply: tokio::sync::oneshot::Sender<serde_json::Value>,
    },
}

/// Shared daemon state exposed to HTTP handlers.
pub struct DaemonState {
    pub agents: HashMap<String, ManagedAgentRef>,
    pub daemon_tx: tokio::sync::mpsc::Sender<DaemonCommand>,
    pub rate_limits: HashMap<String, RateLimitBucket>,
}

/// Lightweight reference to a managed agent for HTTP handlers.
pub struct ManagedAgentRef {
    pub handle: AgentHandle,
    pub state: AgentState,
    pub gateway: Option<ResolvedGatewayConfig>,
}

/// Resolved gateway config (env vars already expanded).
#[derive(Clone)]
pub struct ResolvedGatewayConfig {
    pub enabled: bool,
    pub api_key: Option<String>,
    pub rate_limit: Option<u32>,
}

/// Simple sliding-window rate limiter (per agent, requests per minute).
pub struct RateLimitBucket {
    window: VecDeque<Instant>,
    limit: u32,
}

impl RateLimitBucket {
    pub fn new(limit: u32) -> Self {
        Self {
            window: VecDeque::new(),
            limit,
        }
    }

    pub fn check_and_record(&mut self) -> bool {
        let now = Instant::now();
        let one_minute_ago = now - Duration::from_secs(60);
        while self
            .window
            .front()
            .map(|t| *t < one_minute_ago)
            .unwrap_or(false)
        {
            self.window.pop_front();
        }
        if self.window.len() as u32 >= self.limit {
            return false;
        }
        self.window.push_back(now);
        true
    }

    pub fn remaining(&self) -> u32 {
        let now = Instant::now();
        let one_minute_ago = now - Duration::from_secs(60);
        let active = self.window.iter().filter(|t| **t >= one_minute_ago).count() as u32;
        self.limit.saturating_sub(active)
    }
}

pub type SharedState = Arc<RwLock<DaemonState>>;

pub fn new_shared_state(daemon_tx: tokio::sync::mpsc::Sender<DaemonCommand>) -> SharedState {
    Arc::new(RwLock::new(DaemonState {
        agents: HashMap::new(),
        daemon_tx,
        rate_limits: HashMap::new(),
    }))
}

#[derive(Serialize)]
struct AgentInfo {
    name: String,
    status: String,
    restart_count: u32,
    tokens_used: u64,
    messages_handled: u64,
    uptime_secs: Option<u64>,
    last_error: Option<String>,
    pid: Option<u32>,
}

#[derive(Serialize)]
struct StatusResponse {
    agents: Vec<AgentInfo>,
}

#[derive(Deserialize)]
pub struct ChatRequest {
    pub message: String,
}

#[derive(Serialize)]
struct ChatResponse {
    agent: String,
    response: String,
}

#[derive(Serialize)]
struct ErrorResponse {
    error: String,
}

#[derive(Serialize)]
struct OkResponse {
    status: String,
}

#[derive(Deserialize)]
struct RunSubmitRequest {
    task: String,
}

#[derive(Serialize)]
struct RunSubmitResponse {
    run_id: String,
}

fn agent_to_info(name: &str, state: &AgentState) -> AgentInfo {
    AgentInfo {
        name: name.to_string(),
        status: format!("{}", state.status),
        restart_count: state.restart_count,
        tokens_used: state.tokens_used,
        messages_handled: state.messages_handled,
        uptime_secs: state.started_at.map(|t| t.elapsed().as_secs()),
        last_error: state.last_error.clone(),
        pid: state.pid,
    }
}

/// Build the axum router.
pub fn build_router(state: SharedState) -> Router {
    Router::new()
        .route("/status", get(get_status))
        .route("/agents/{name}/chat", post(post_chat))
        .route("/agents/{name}/stop", post(post_stop))
        .route("/agents/{name}/start", post(post_start))
        .route("/agents/{name}/restart", post(post_restart))
        .route("/agents/{name}/status", get(get_agent_status))
        .route("/agents/{name}/logs", get(get_agent_logs))
        .route("/orchestrate/{name}", post(post_orchestrate))
        .route("/gw/{name}/chat", post(gateway_chat))
        .route("/health", get(get_health))
        .route("/runs", post(post_run_submit).get(get_runs_list))
        .route("/runs/{id}", get(get_run_status))
        .route("/runs/{id}/result", get(get_run_result))
        .route("/runs/{id}/cancel", post(post_run_cancel))
        .route("/metrics", get(get_metrics))
        .with_state(state)
}

async fn get_status(State(state): State<SharedState>) -> Json<StatusResponse> {
    let s = state.read().await;
    let agents: Vec<AgentInfo> = s
        .agents
        .iter()
        .map(|(name, m)| agent_to_info(name, &m.state))
        .collect();
    Json(StatusResponse { agents })
}

async fn get_agent_status(
    State(state): State<SharedState>,
    Path(name): Path<String>,
) -> Result<Json<AgentInfo>, (StatusCode, Json<ErrorResponse>)> {
    let s = state.read().await;
    match s.agents.get(&name) {
        Some(m) => Ok(Json(agent_to_info(&name, &m.state))),
        None => Err((
            StatusCode::NOT_FOUND,
            Json(ErrorResponse {
                error: format!("agent '{}' not found", name),
            }),
        )),
    }
}

#[derive(Serialize)]
struct LogsResponse {
    agent: String,
    logs: Vec<LogEntry>,
}

async fn get_agent_logs(
    State(state): State<SharedState>,
    Path(name): Path<String>,
) -> Result<Json<LogsResponse>, (StatusCode, Json<ErrorResponse>)> {
    let s = state.read().await;
    match s.agents.get(&name) {
        Some(m) => Ok(Json(LogsResponse {
            agent: name,
            logs: m.state.logs.clone(),
        })),
        None => Err((
            StatusCode::NOT_FOUND,
            Json(ErrorResponse {
                error: format!("agent '{}' not found", name),
            }),
        )),
    }
}

async fn post_chat(
    State(state): State<SharedState>,
    Path(name): Path<String>,
    Json(body): Json<ChatRequest>,
) -> Result<Json<ChatResponse>, (StatusCode, Json<ErrorResponse>)> {
    let handle = {
        let s = state.read().await;
        match s.agents.get(&name) {
            Some(m) => m.handle.clone(),
            None => {
                return Err((
                    StatusCode::NOT_FOUND,
                    Json(ErrorResponse {
                        error: format!("agent '{}' not found", name),
                    }),
                ));
            }
        }
    };

    match handle.chat(body.message).await {
        Ok(content) => Ok(Json(ChatResponse {
            agent: name,
            response: content,
        })),
        Err(e) => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse { error: e }),
        )),
    }
}

async fn post_stop(
    State(state): State<SharedState>,
    Path(name): Path<String>,
) -> Result<Json<OkResponse>, (StatusCode, Json<ErrorResponse>)> {
    let handle = {
        let s = state.read().await;
        match s.agents.get(&name) {
            Some(m) => m.handle.clone(),
            None => {
                return Err((
                    StatusCode::NOT_FOUND,
                    Json(ErrorResponse {
                        error: format!("agent '{}' not found", name),
                    }),
                ));
            }
        }
    };

    handle.stop().await;

    // Update state
    {
        let mut s = state.write().await;
        if let Some(m) = s.agents.get_mut(&name) {
            m.state.status = AgentStatus::Stopped;
        }
    }

    Ok(Json(OkResponse {
        status: format!("agent '{}' stop signal sent", name),
    }))
}

async fn post_start(
    State(state): State<SharedState>,
    Path(name): Path<String>,
) -> Result<Json<OkResponse>, (StatusCode, Json<ErrorResponse>)> {
    let daemon_tx = {
        let s = state.read().await;
        // Check if already running
        if let Some(m) = s.agents.get(&name) {
            if m.state.status != AgentStatus::Stopped && m.state.status != AgentStatus::Error {
                return Err((
                    StatusCode::CONFLICT,
                    Json(ErrorResponse {
                        error: format!("agent '{}' is already {}", name, m.state.status),
                    }),
                ));
            }
        }
        s.daemon_tx.clone()
    };

    let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
    daemon_tx
        .send(DaemonCommand::Start {
            name: name.clone(),
            reply: reply_tx,
        })
        .await
        .map_err(|_| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse {
                    error: "daemon command channel closed".into(),
                }),
            )
        })?;

    match reply_rx.await {
        Ok(Ok(msg)) => Ok(Json(OkResponse { status: msg })),
        Ok(Err(e)) => Err((StatusCode::BAD_REQUEST, Json(ErrorResponse { error: e }))),
        Err(_) => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse {
                error: "daemon did not reply".into(),
            }),
        )),
    }
}

async fn post_restart(
    State(state): State<SharedState>,
    Path(name): Path<String>,
) -> Result<Json<OkResponse>, (StatusCode, Json<ErrorResponse>)> {
    let daemon_tx = {
        let s = state.read().await;
        s.daemon_tx.clone()
    };

    let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
    daemon_tx
        .send(DaemonCommand::Restart {
            name: name.clone(),
            reply: reply_tx,
        })
        .await
        .map_err(|_| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse {
                    error: "daemon command channel closed".into(),
                }),
            )
        })?;

    match reply_rx.await {
        Ok(Ok(msg)) => Ok(Json(OkResponse { status: msg })),
        Ok(Err(e)) => Err((StatusCode::BAD_REQUEST, Json(ErrorResponse { error: e }))),
        Err(_) => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse {
                error: "daemon did not reply".into(),
            }),
        )),
    }
}

#[derive(Deserialize)]
struct OrchestrateRequest {
    message: String,
    #[serde(default = "default_max_rounds")]
    max_rounds: usize,
}

fn default_max_rounds() -> usize {
    5
}

#[derive(Serialize)]
struct OrchestrateResponse {
    agent: String,
    response: String,
    delegations: Vec<DelegationResult>,
    rounds: usize,
}

#[derive(Serialize, Clone)]
struct DelegationResult {
    to: String,
    query: String,
    result: String,
}

/// Parse `@delegate(agent_name): message` markers from LLM response.
fn parse_delegations(text: &str) -> Vec<(String, String)> {
    let mut delegations = Vec::new();
    for line in text.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("@delegate(") {
            if let Some(paren_end) = rest.find(')') {
                let agent_name = &rest[..paren_end];
                let message = rest[paren_end + 1..].trim_start_matches(':').trim();
                if !agent_name.is_empty() && !message.is_empty() {
                    delegations.push((agent_name.to_string(), message.to_string()));
                }
            }
        }
    }
    delegations
}

async fn post_orchestrate(
    State(state): State<SharedState>,
    Path(name): Path<String>,
    Json(body): Json<OrchestrateRequest>,
) -> Result<Json<OrchestrateResponse>, (StatusCode, Json<ErrorResponse>)> {
    let max_rounds = body.max_rounds.min(10);
    let mut all_delegations: Vec<DelegationResult> = Vec::new();
    let mut current_message = body.message;
    let mut rounds = 0;
    let final_response;

    loop {
        rounds += 1;

        // Send to manager
        let manager_handle = {
            let s = state.read().await;
            s.agents.get(&name).map(|m| m.handle.clone())
        }
        .ok_or_else(|| {
            (
                StatusCode::NOT_FOUND,
                Json(ErrorResponse {
                    error: format!("agent '{}' not found", name),
                }),
            )
        })?;

        let response = manager_handle.chat(current_message).await.map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse { error: e }),
            )
        })?;

        let delegations = parse_delegations(&response);

        if delegations.is_empty() || rounds > max_rounds {
            final_response = response;
            break;
        }

        // Execute delegations
        let mut results = Vec::new();
        for (target_name, target_msg) in &delegations {
            let target_handle = {
                let s = state.read().await;
                s.agents.get(target_name).map(|m| m.handle.clone())
            };

            match target_handle {
                Some(handle) => match handle.chat(target_msg.clone()).await {
                    Ok(result) => {
                        all_delegations.push(DelegationResult {
                            to: target_name.clone(),
                            query: target_msg.clone(),
                            result: result.clone(),
                        });
                        results.push(format!("From @{}:\n{}", target_name, result));
                    }
                    Err(e) => {
                        results.push(format!("From @{}: [error: {}]", target_name, e));
                    }
                },
                None => {
                    results.push(format!(
                        "From @{}: [agent not found or not running]",
                        target_name
                    ));
                }
            }
        }

        // Feed results back to manager
        current_message = format!(
            "Here are the results from the agents you delegated to:\n\n{}\n\nNow provide your final answer incorporating these results. If you need more info, delegate again.",
            results.join("\n\n")
        );
    }

    Ok(Json(OrchestrateResponse {
        agent: name,
        response: final_response,
        delegations: all_delegations,
        rounds,
    }))
}

async fn post_run_submit(
    State(state): State<SharedState>,
    Json(body): Json<RunSubmitRequest>,
) -> Result<Json<RunSubmitResponse>, (StatusCode, Json<ErrorResponse>)> {
    let daemon_tx = { state.read().await.daemon_tx.clone() };
    let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
    daemon_tx
        .send(DaemonCommand::SubmitRun {
            task: body.task,
            reply: reply_tx,
        })
        .await
        .map_err(|_| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse {
                    error: "daemon channel closed".into(),
                }),
            )
        })?;
    match reply_rx.await {
        Ok(Ok(run_id)) => Ok(Json(RunSubmitResponse { run_id })),
        Ok(Err(e)) => Err((StatusCode::BAD_REQUEST, Json(ErrorResponse { error: e }))),
        Err(_) => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse {
                error: "daemon did not reply".into(),
            }),
        )),
    }
}

async fn get_run_status(
    State(state): State<SharedState>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<ErrorResponse>)> {
    let daemon_tx = { state.read().await.daemon_tx.clone() };
    let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
    daemon_tx
        .send(DaemonCommand::GetRunStatus {
            run_id: id,
            reply: reply_tx,
        })
        .await
        .map_err(|_| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse {
                    error: "daemon channel closed".into(),
                }),
            )
        })?;
    match reply_rx.await {
        Ok(Ok(data)) => Ok(Json(data)),
        Ok(Err(e)) => Err((StatusCode::NOT_FOUND, Json(ErrorResponse { error: e }))),
        Err(_) => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse {
                error: "daemon did not reply".into(),
            }),
        )),
    }
}

async fn get_runs_list(
    State(state): State<SharedState>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<ErrorResponse>)> {
    let daemon_tx = { state.read().await.daemon_tx.clone() };
    let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
    daemon_tx
        .send(DaemonCommand::ListRuns { reply: reply_tx })
        .await
        .map_err(|_| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse {
                    error: "daemon channel closed".into(),
                }),
            )
        })?;
    match reply_rx.await {
        Ok(Ok(data)) => Ok(Json(data)),
        Ok(Err(e)) => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse { error: e }),
        )),
        Err(_) => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse {
                error: "daemon did not reply".into(),
            }),
        )),
    }
}

async fn get_run_result(
    State(state): State<SharedState>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<ErrorResponse>)> {
    let daemon_tx = { state.read().await.daemon_tx.clone() };
    let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
    daemon_tx
        .send(DaemonCommand::GetRunResult {
            run_id: id,
            reply: reply_tx,
        })
        .await
        .map_err(|_| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse {
                    error: "daemon channel closed".into(),
                }),
            )
        })?;
    match reply_rx.await {
        Ok(Ok(data)) => Ok(Json(data)),
        Ok(Err(e)) => Err((StatusCode::NOT_FOUND, Json(ErrorResponse { error: e }))),
        Err(_) => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse {
                error: "daemon did not reply".into(),
            }),
        )),
    }
}

async fn post_run_cancel(
    State(state): State<SharedState>,
    Path(id): Path<String>,
) -> Result<Json<OkResponse>, (StatusCode, Json<ErrorResponse>)> {
    let daemon_tx = { state.read().await.daemon_tx.clone() };
    let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
    daemon_tx
        .send(DaemonCommand::CancelRun {
            run_id: id,
            reply: reply_tx,
        })
        .await
        .map_err(|_| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse {
                    error: "daemon channel closed".into(),
                }),
            )
        })?;
    match reply_rx.await {
        Ok(Ok(msg)) => Ok(Json(OkResponse { status: msg })),
        Ok(Err(e)) => Err((StatusCode::NOT_FOUND, Json(ErrorResponse { error: e }))),
        Err(_) => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse {
                error: "daemon did not reply".into(),
            }),
        )),
    }
}

async fn get_metrics(State(state): State<SharedState>) -> Json<serde_json::Value> {
    let daemon_tx = { state.read().await.daemon_tx.clone() };
    let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
    let _ = daemon_tx
        .send(DaemonCommand::GetMetrics { reply: reply_tx })
        .await;
    match reply_rx.await {
        Ok(data) => Json(data),
        Err(_) => Json(serde_json::json!({"error": "daemon did not reply"})),
    }
}

// --- Health endpoint (for external gateways / load balancers) ---

#[derive(Serialize)]
struct HealthResponse {
    status: String,
    agents: usize,
}

async fn get_health(State(state): State<SharedState>) -> Json<HealthResponse> {
    let s = state.read().await;
    let running = s
        .agents
        .values()
        .filter(|m| m.state.status == AgentStatus::Running || m.state.status == AgentStatus::Idle)
        .count();
    Json(HealthResponse {
        status: "ok".into(),
        agents: running,
    })
}

// --- Gateway endpoint (built-in API key auth + rate limiting) ---

#[derive(Serialize)]
struct GatewayResponse {
    agent: String,
    response: String,
}

#[derive(Serialize)]
struct GatewayErrorResponse {
    error: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    retry_after_secs: Option<u32>,
}

fn extract_bearer_token(headers: &HeaderMap) -> Option<&str> {
    headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
}

async fn gateway_chat(
    State(state): State<SharedState>,
    Path(name): Path<String>,
    headers: HeaderMap,
    Json(body): Json<ChatRequest>,
) -> Result<Json<GatewayResponse>, (StatusCode, Json<GatewayErrorResponse>)> {
    let gw_err = |status: StatusCode, msg: &str| {
        (
            status,
            Json(GatewayErrorResponse {
                error: msg.into(),
                retry_after_secs: None,
            }),
        )
    };

    // Look up agent + gateway config
    let (handle, gateway_config) = {
        let s = state.read().await;
        match s.agents.get(&name) {
            Some(m) => (m.handle.clone(), m.gateway.clone()),
            None => {
                return Err(gw_err(
                    StatusCode::NOT_FOUND,
                    &format!("agent '{}' not found", name),
                ));
            }
        }
    };

    // Check gateway is enabled
    let gw = match gateway_config {
        Some(g) if g.enabled => g,
        _ => {
            return Err(gw_err(
                StatusCode::FORBIDDEN,
                &format!("gateway not enabled for agent '{}'", name),
            ));
        }
    };

    // API key auth
    if let Some(ref expected_key) = gw.api_key {
        match extract_bearer_token(&headers) {
            Some(token) if token == expected_key => {}
            Some(_) => return Err(gw_err(StatusCode::UNAUTHORIZED, "invalid API key")),
            None => {
                return Err(gw_err(
                    StatusCode::UNAUTHORIZED,
                    "missing Authorization: Bearer <key> header",
                ));
            }
        }
    }

    // Rate limiting
    if let Some(limit) = gw.rate_limit {
        let mut s = state.write().await;
        let bucket = s
            .rate_limits
            .entry(name.clone())
            .or_insert_with(|| RateLimitBucket::new(limit));
        if !bucket.check_and_record() {
            return Err((
                StatusCode::TOO_MANY_REQUESTS,
                Json(GatewayErrorResponse {
                    error: format!("rate limit exceeded ({}/min)", limit),
                    retry_after_secs: Some(60),
                }),
            ));
        }
    }

    // Proxy to agent
    match handle.chat(body.message).await {
        Ok(content) => Ok(Json(GatewayResponse {
            agent: name,
            response: content,
        })),
        Err(e) => Err(gw_err(StatusCode::INTERNAL_SERVER_ERROR, &e)),
    }
}

/// Start the HTTP server on the given bind address.
pub async fn start_server(bind: String, state: SharedState) {
    let app = build_router(state);
    let listener = tokio::net::TcpListener::bind(&bind).await.unwrap();
    info!(bind = %bind, "HTTP server listening");
    axum::serve(listener, app).await.unwrap();
}
