//! HTTP server — REST API for agent management.

use std::collections::HashMap;
use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::Json;
use axum::routing::{get, post};
use axum::Router;
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
}

/// Shared daemon state exposed to HTTP handlers.
pub struct DaemonState {
  pub agents: HashMap<String, ManagedAgentRef>,
  pub daemon_tx: tokio::sync::mpsc::Sender<DaemonCommand>,
}

/// Lightweight reference to a managed agent for HTTP handlers.
pub struct ManagedAgentRef {
  pub handle: AgentHandle,
  pub state: AgentState,
}

pub type SharedState = Arc<RwLock<DaemonState>>;

pub fn new_shared_state(
  daemon_tx: tokio::sync::mpsc::Sender<DaemonCommand>,
) -> SharedState {
  Arc::new(RwLock::new(DaemonState {
    agents: HashMap::new(),
    daemon_tx,
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

fn agent_to_info(name: &str, state: &AgentState) -> AgentInfo {
  AgentInfo {
    name: name.to_string(),
    status: format!("{}", state.status),
    restart_count: state.restart_count,
    tokens_used: state.tokens_used,
    messages_handled: state.messages_handled,
    uptime_secs: state.started_at.map(|t| t.elapsed().as_secs()),
    last_error: state.last_error.clone(),
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
        ))
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
        ))
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
    Ok(Err(e)) => Err((
      StatusCode::BAD_REQUEST,
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
    Ok(Err(e)) => Err((
      StatusCode::BAD_REQUEST,
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

/// Start the HTTP server on the given bind address.
pub async fn start_server(bind: String, state: SharedState) {
  let app = build_router(state);
  let listener = tokio::net::TcpListener::bind(&bind).await.unwrap();
  info!(bind = %bind, "HTTP server listening");
  axum::serve(listener, app).await.unwrap();
}
