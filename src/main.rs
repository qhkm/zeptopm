use clap::Parser;
#[derive(Parser, Debug)]
#[command(
    name = "zeptopm",
    version,
    about = "Process manager for AI agents — like PM2, but for LLMs"
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,

    /// Config file path
    #[arg(short, long, default_value = "zeptopm.toml", global = true)]
    config: String,

    /// Override log level (trace|debug|info|warn|error)
    #[arg(short, long, global = true)]
    log_level: Option<String>,

    /// Daemon HTTP address (for CLI commands to connect to)
    #[arg(long, default_value = "127.0.0.1:9876", global = true)]
    addr: String,

    /// Output results as JSON (machine-readable)
    #[arg(long, global = true)]
    json: bool,

    /// Show command schema for AI agents (JSON)
    #[arg(long, global = true)]
    agent_help: bool,
}

#[derive(clap::Subcommand, Debug)]
enum Commands {
    /// Start the daemon — runs all auto_start agents
    Daemon {
        /// Override server bind address
        #[arg(short, long)]
        bind: Option<String>,
    },
    /// Show status of all running agents (queries daemon)
    Status,
    /// List configured agents (from config file, no daemon needed)
    List,
    /// Send a message to an agent and get the response
    Chat {
        /// Agent name
        name: String,
        /// Message to send
        message: String,
    },
    /// Stop a running agent
    Stop {
        /// Agent name
        name: String,
    },
    /// Start an agent (must be defined in config)
    Start {
        /// Agent name
        name: String,
    },
    /// Restart an agent (stop + start)
    Restart {
        /// Agent name
        name: String,
    },
    /// Show recent logs for an agent
    Logs {
        /// Agent name
        name: String,
    },
    /// Chain agents in a pipeline (output of one feeds into the next)
    Pipeline {
        /// Comma-separated agent names (e.g. "researcher,writer")
        agents: String,
        /// Message to start the pipeline
        message: String,
    },
    /// Orchestrate multi-agent collaboration (manager delegates to other agents)
    Orchestrate {
        /// Manager agent name (coordinates the work)
        manager: String,
        /// Task for the manager
        message: String,
    },
    /// Submit and manage orchestrated multi-agent runs
    Run {
        #[command(subcommand)]
        action: RunAction,
    },
    /// Show full CLI manifest for AI agents (JSON)
    AgentHelp,
    /// Internal: run a single agent as a worker process
    #[command(hide = true)]
    Worker {
        /// Agent name
        #[arg(long)]
        agent: String,
        /// Config file path
        #[arg(long)]
        config: String,
    },
}

#[derive(clap::Subcommand, Debug)]
enum RunAction {
    /// Submit a new orchestrated run
    Submit {
        /// Task description
        task: String,
        /// Stream run progress in real-time
        #[arg(short, long)]
        tail: bool,
    },
    /// Check status of a run
    Status {
        /// Run ID
        run_id: String,
        /// Stream run progress in real-time
        #[arg(short, long)]
        tail: bool,
    },
    /// List all runs
    List,
    /// Print final artifact content for a completed run
    Result {
        /// Run ID
        run_id: String,
    },
    /// Cancel a running run (cancels all active jobs)
    Cancel {
        /// Run ID
        run_id: String,
    },
}

fn init_tracing(level: &str, format: &str) {
    let env_filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(level));

    match format {
        "json" => {
            tracing_subscriber::fmt()
                .json()
                .with_env_filter(env_filter)
                .init();
        }
        "compact" => {
            tracing_subscriber::fmt()
                .compact()
                .with_env_filter(env_filter)
                .init();
        }
        _ => {
            tracing_subscriber::fmt().with_env_filter(env_filter).init();
        }
    }
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();

    if cli.agent_help {
        let manifest = build_manifest();
        let cmd_name = match &cli.command {
            Some(Commands::Status) => Some("status"),
            Some(Commands::List) => Some("list"),
            Some(Commands::Chat { .. }) => Some("chat"),
            Some(Commands::Stop { .. }) => Some("stop"),
            Some(Commands::Start { .. }) => Some("start"),
            Some(Commands::Restart { .. }) => Some("restart"),
            Some(Commands::Logs { .. }) => Some("logs"),
            Some(Commands::Pipeline { .. }) => Some("pipeline"),
            Some(Commands::Orchestrate { .. }) => Some("orchestrate"),
            Some(Commands::Run { action }) => Some(match action {
                RunAction::Submit { .. } => "run submit",
                RunAction::Status { .. } => "run status",
                RunAction::List => "run list",
                RunAction::Result { .. } => "run result",
                RunAction::Cancel { .. } => "run cancel",
            }),
            _ => None,
        };

        if let Some(name) = cmd_name {
            if let Some(cmd_spec) = manifest["commands"].get(name) {
                let mut entry = serde_json::Map::new();
                entry.insert(name.to_string(), cmd_spec.clone());
                let entry = serde_json::Value::Object(entry);
                println!("{}", serde_json::to_string_pretty(&entry).unwrap());
            } else {
                println!("{}", serde_json::to_string_pretty(&manifest).unwrap());
            }
        } else {
            println!("{}", serde_json::to_string_pretty(&manifest).unwrap());
        }
        return;
    }

    match cli.command {
        Some(Commands::Daemon { bind }) => {
            // Load config to get log settings
            let config = zeptopm::config::load_config(&cli.config).unwrap_or_else(|e| {
                eprintln!("Failed to load config: {}", e);
                std::process::exit(1);
            });

            let log_level = cli.log_level.as_deref().unwrap_or(&config.daemon.log_level);
            init_tracing(log_level, &config.daemon.log_format);

            zeptopm::daemon::run(cli.config, bind).await;
        }
        Some(Commands::Status) => {
            let result: CliResult = match http_get(&cli.addr, "/status").await {
                Ok(body) => serde_json::from_str::<serde_json::Value>(&body)
                    .map_err(|e| CliError::parse_error(&e.to_string())),
                Err(_) => Err(CliError::daemon_unreachable(&cli.addr)),
            };
            output_result(result, cli.json, |data| {
                if let Some(agents) = data.get("agents").and_then(|a| a.as_array()) {
                    if agents.is_empty() {
                        println!("No agents running.");
                        return;
                    }
                    println!(
                        "{:<20} {:<15} {:<8} {:<12} {}",
                        "NAME", "STATUS", "RESTARTS", "TOKENS", "UPTIME"
                    );
                    println!("{}", "-".repeat(70));
                    for agent in agents {
                        let name = agent.get("name").and_then(|v| v.as_str()).unwrap_or("?");
                        let status = agent.get("status").and_then(|v| v.as_str()).unwrap_or("?");
                        let restarts = agent
                            .get("restart_count")
                            .and_then(|v| v.as_u64())
                            .unwrap_or(0);
                        let tokens = agent
                            .get("tokens_used")
                            .and_then(|v| v.as_u64())
                            .unwrap_or(0);
                        let uptime = agent
                            .get("uptime_secs")
                            .and_then(|v| v.as_u64())
                            .map(format_uptime)
                            .unwrap_or_else(|| "-".into());
                        println!(
                            "{:<20} {:<15} {:<8} {:<12} {}",
                            name, status, restarts, tokens, uptime
                        );
                    }
                }
            });
        }
        Some(Commands::List) => {
            let result: CliResult = match zeptopm::config::load_config(&cli.config) {
                Ok(config) => {
                    let agents: Vec<serde_json::Value> = config
                        .agents
                        .iter()
                        .map(|a| {
                            serde_json::json!({
                              "name": a.name,
                              "auto_start": a.auto_start,
                              "provider": a.provider,
                              "model": a.model.as_deref().unwrap_or("default"),
                            })
                        })
                        .collect();
                    Ok(serde_json::json!({ "agents": agents }))
                }
                Err(e) => Err(CliError::invalid_config(&e.to_string())),
            };
            output_result(result, cli.json, |data| {
                if let Some(agents) = data.get("agents").and_then(|v| v.as_array()) {
                    for agent in agents {
                        let name = agent.get("name").and_then(|v| v.as_str()).unwrap_or("?");
                        let auto = if agent
                            .get("auto_start")
                            .and_then(|v| v.as_bool())
                            .unwrap_or(false)
                        {
                            "auto"
                        } else {
                            "manual"
                        };
                        let provider = agent
                            .get("provider")
                            .and_then(|v| v.as_str())
                            .unwrap_or("?");
                        let model = agent
                            .get("model")
                            .and_then(|v| v.as_str())
                            .unwrap_or("default");
                        println!(
                            "{:<20} {:<10} provider={:<15} model={}",
                            name, auto, provider, model
                        );
                    }
                }
            });
        }
        Some(Commands::Chat { name, message }) => {
            let body = serde_json::json!({ "message": message });
            let result: CliResult =
                match http_post(&cli.addr, &format!("/agents/{}/chat", name), &body).await {
                    Ok(resp_body) => match serde_json::from_str::<serde_json::Value>(&resp_body) {
                        Ok(resp) => {
                            if resp.get("error").and_then(|v| v.as_str()).is_some() {
                                Err(CliError::not_found("agent", &name))
                            } else {
                                Ok(resp)
                            }
                        }
                        Err(e) => Err(CliError::parse_error(&e.to_string())),
                    },
                    Err(_) => Err(CliError::daemon_unreachable(&cli.addr)),
                };
            output_result(result, cli.json, |data| {
                if let Some(content) = data.get("response").and_then(|v| v.as_str()) {
                    println!("{}", content);
                }
            });
        }
        Some(Commands::Stop { name }) => {
            let result: CliResult = match http_post(
                &cli.addr,
                &format!("/agents/{}/stop", name),
                &serde_json::json!({}),
            )
            .await
            {
                Ok(resp_body) => match serde_json::from_str::<serde_json::Value>(&resp_body) {
                    Ok(resp) => {
                        if resp.get("error").and_then(|v| v.as_str()).is_some() {
                            Err(CliError::not_found("agent", &name))
                        } else {
                            Ok(resp)
                        }
                    }
                    Err(e) => Err(CliError::parse_error(&e.to_string())),
                },
                Err(_) => Err(CliError::daemon_unreachable(&cli.addr)),
            };
            output_result(result, cli.json, |data| {
                if let Some(status) = data.get("status").and_then(|v| v.as_str()) {
                    println!("{}", status);
                }
            });
        }
        Some(Commands::Start { name }) => {
            let result: CliResult = match http_post(
                &cli.addr,
                &format!("/agents/{}/start", name),
                &serde_json::json!({}),
            )
            .await
            {
                Ok(resp_body) => match serde_json::from_str::<serde_json::Value>(&resp_body) {
                    Ok(resp) => {
                        if resp.get("error").and_then(|v| v.as_str()).is_some() {
                            Err(CliError::not_found("agent", &name))
                        } else {
                            Ok(resp)
                        }
                    }
                    Err(e) => Err(CliError::parse_error(&e.to_string())),
                },
                Err(_) => Err(CliError::daemon_unreachable(&cli.addr)),
            };
            output_result(result, cli.json, |data| {
                if let Some(status) = data.get("status").and_then(|v| v.as_str()) {
                    println!("{}", status);
                }
            });
        }
        Some(Commands::Restart { name }) => {
            let result: CliResult = match http_post(
                &cli.addr,
                &format!("/agents/{}/restart", name),
                &serde_json::json!({}),
            )
            .await
            {
                Ok(resp_body) => match serde_json::from_str::<serde_json::Value>(&resp_body) {
                    Ok(resp) => {
                        if resp.get("error").and_then(|v| v.as_str()).is_some() {
                            Err(CliError::not_found("agent", &name))
                        } else {
                            Ok(resp)
                        }
                    }
                    Err(e) => Err(CliError::parse_error(&e.to_string())),
                },
                Err(_) => Err(CliError::daemon_unreachable(&cli.addr)),
            };
            output_result(result, cli.json, |data| {
                if let Some(status) = data.get("status").and_then(|v| v.as_str()) {
                    println!("{}", status);
                }
            });
        }
        Some(Commands::Pipeline { agents, message }) => {
            let agent_names: Vec<&str> = agents.split(',').map(|s| s.trim()).collect();
            if agent_names.is_empty() {
                let result: CliResult = Err(CliError {
                    message: "No agents specified".into(),
                    code: "INVALID_CONFIG".into(),
                });
                output_result(result, cli.json, |_| {});
                return;
            }

            let mut steps = Vec::new();
            let mut current_message = message.clone();
            let mut failed = false;

            for (i, agent_name) in agent_names.iter().enumerate() {
                let body = serde_json::json!({ "message": current_message });
                match http_post(&cli.addr, &format!("/agents/{}/chat", agent_name), &body).await {
                    Ok(resp_body) => {
                        let resp: serde_json::Value =
                            serde_json::from_str(&resp_body).unwrap_or_default();
                        if let Some(error) = resp.get("error").and_then(|v| v.as_str()) {
                            let result: CliResult = Err(CliError::not_found("agent", agent_name));
                            output_result(result, cli.json, |_| {
                                eprintln!("Error from {}: {}", agent_name, error);
                            });
                            failed = true;
                            break;
                        }
                        let response = resp
                            .get("response")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                        steps
                            .push(serde_json::json!({ "agent": agent_name, "response": response }));
                        if i < agent_names.len() - 1 {
                            current_message = format!(
                                "Previous step (from {}): {}\n\nContinue with the original task: {}",
                                agent_name, response, message
                            );
                        }
                    }
                    Err(_) => {
                        let result: CliResult = Err(CliError::daemon_unreachable(&cli.addr));
                        output_result(result, cli.json, |_| {});
                        failed = true;
                        break;
                    }
                }
            }

            if !failed {
                let result: CliResult = Ok(serde_json::json!({ "steps": steps }));
                output_result(result, cli.json, |data| {
                    if let Some(steps) = data.get("steps").and_then(|v| v.as_array()) {
                        for (i, step) in steps.iter().enumerate() {
                            let agent = step.get("agent").and_then(|v| v.as_str()).unwrap_or("?");
                            let response =
                                step.get("response").and_then(|v| v.as_str()).unwrap_or("");
                            println!("--- [{}] {} ---", i + 1, agent);
                            println!("{}\n", response);
                        }
                    }
                });
            }
        }
        Some(Commands::Orchestrate { manager, message }) => {
            let body = serde_json::json!({ "message": message });
            let result: CliResult =
                match http_post(&cli.addr, &format!("/orchestrate/{}", manager), &body).await {
                    Ok(resp_body) => match serde_json::from_str::<serde_json::Value>(&resp_body) {
                        Ok(resp) => {
                            if resp.get("error").and_then(|v| v.as_str()).is_some() {
                                Err(CliError::not_found("agent", &manager))
                            } else {
                                Ok(resp)
                            }
                        }
                        Err(e) => Err(CliError::parse_error(&e.to_string())),
                    },
                    Err(_) => Err(CliError::daemon_unreachable(&cli.addr)),
                };
            output_result(result, cli.json, |data| {
                if let Some(delegations) = data.get("delegations").and_then(|v| v.as_array()) {
                    if !delegations.is_empty() {
                        println!("--- delegations ---");
                        for d in delegations {
                            let to = d.get("to").and_then(|v| v.as_str()).unwrap_or("?");
                            let query = d.get("query").and_then(|v| v.as_str()).unwrap_or("?");
                            let result = d.get("result").and_then(|v| v.as_str()).unwrap_or("?");
                            println!("  -> @{}: {}", to, query);
                            println!("  <- {}", result);
                            println!();
                        }
                        println!("--- final response ---");
                    }
                }
                if let Some(response) = data.get("response").and_then(|v| v.as_str()) {
                    println!("{}", response);
                }
                if let Some(rounds) = data.get("rounds").and_then(|v| v.as_u64()) {
                    if rounds > 1 {
                        eprintln!("\n({} rounds)", rounds);
                    }
                }
            });
        }
        Some(Commands::Run { action }) => match action {
            RunAction::Submit { task, tail } => {
                let body = serde_json::json!({ "task": task });
                let result: CliResult = match http_post(&cli.addr, "/runs", &body).await {
                    Ok(resp_body) => match serde_json::from_str::<serde_json::Value>(&resp_body) {
                        Ok(resp) => {
                            if let Some(error) = resp.get("error").and_then(|v| v.as_str()) {
                                Err(CliError::parse_error(error))
                            } else {
                                Ok(resp)
                            }
                        }
                        Err(e) => Err(CliError::parse_error(&e.to_string())),
                    },
                    Err(_) => Err(CliError::daemon_unreachable(&cli.addr)),
                };

                let run_id = result
                    .as_ref()
                    .ok()
                    .and_then(|d| d.get("run_id"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();

                output_result(result, cli.json, |data| {
                    if let Some(id) = data.get("run_id").and_then(|v| v.as_str()) {
                        println!("Run submitted: {}", id);
                    }
                });

                if tail && !run_id.is_empty() {
                    tail_run(&cli.addr, &run_id).await;
                }
            }
            RunAction::Status { run_id, tail } => {
                let result: CliResult = match http_get(&cli.addr, &format!("/runs/{}", run_id))
                    .await
                {
                    Ok(resp_body) => match serde_json::from_str::<serde_json::Value>(&resp_body) {
                        Ok(resp) => {
                            if resp.get("error").and_then(|v| v.as_str()).is_some() {
                                Err(CliError::not_found("run", &run_id))
                            } else {
                                Ok(resp)
                            }
                        }
                        Err(e) => Err(CliError::parse_error(&e.to_string())),
                    },
                    Err(_) => Err(CliError::daemon_unreachable(&cli.addr)),
                };
                output_result(result, cli.json, |data| {
                    let body = serde_json::to_string(data).unwrap_or_default();
                    print_run_status(&body);
                });
                if tail {
                    tail_run(&cli.addr, &run_id).await;
                }
            }
            RunAction::List => {
                let result: CliResult = match http_get(&cli.addr, "/runs").await {
                    Ok(resp_body) => serde_json::from_str::<serde_json::Value>(&resp_body)
                        .map_err(|e| CliError::parse_error(&e.to_string())),
                    Err(_) => Err(CliError::daemon_unreachable(&cli.addr)),
                };
                output_result(result, cli.json, |data| {
                    if let Some(runs) = data.get("runs").and_then(|v| v.as_array()) {
                        if runs.is_empty() {
                            println!("No runs.");
                            return;
                        }
                        println!("{:<24} {:<12} {}", "RUN ID", "STATUS", "TASK");
                        println!("{}", "-".repeat(70));
                        for run in runs {
                            let id = run.get("run_id").and_then(|v| v.as_str()).unwrap_or("?");
                            let status = run.get("status").and_then(|v| v.as_str()).unwrap_or("?");
                            let task = run.get("task").and_then(|v| v.as_str()).unwrap_or("");
                            let short_task: String = task.chars().take(40).collect();
                            println!("{:<24} {:<12} {}", id, status, short_task);
                        }
                    }
                });
            }
            RunAction::Result { run_id } => {
                let result: CliResult =
                    match http_get(&cli.addr, &format!("/runs/{}/result", run_id)).await {
                        Ok(resp_body) => {
                            match serde_json::from_str::<serde_json::Value>(&resp_body) {
                                Ok(resp) => {
                                    if resp.get("error").and_then(|v| v.as_str()).is_some() {
                                        Err(CliError::not_found("run", &run_id))
                                    } else {
                                        Ok(resp)
                                    }
                                }
                                Err(e) => Err(CliError::parse_error(&e.to_string())),
                            }
                        }
                        Err(_) => Err(CliError::daemon_unreachable(&cli.addr)),
                    };
                output_result(result, cli.json, |data| {
                    let status = data.get("status").and_then(|v| v.as_str()).unwrap_or("?");
                    println!("Run: {}  Status: {}", run_id, status);
                    if let Some(artifacts) = data.get("artifacts").and_then(|v| v.as_array()) {
                        for artifact in artifacts {
                            let kind = artifact.get("kind").and_then(|v| v.as_str()).unwrap_or("?");
                            let summary = artifact
                                .get("summary")
                                .and_then(|v| v.as_str())
                                .unwrap_or("");
                            let path = artifact.get("path").and_then(|v| v.as_str()).unwrap_or("");
                            println!("\n--- artifact ({}) ---", kind);
                            if !summary.is_empty() {
                                println!("Summary: {}", summary);
                            }
                            if !path.is_empty() {
                                if let Ok(content) = std::fs::read_to_string(path) {
                                    println!("{}", content);
                                } else {
                                    println!("(file: {})", path);
                                }
                            }
                        }
                    }
                });
            }
            RunAction::Cancel { run_id } => {
                let result: CliResult = match http_post(
                    &cli.addr,
                    &format!("/runs/{}/cancel", run_id),
                    &serde_json::json!({}),
                )
                .await
                {
                    Ok(resp_body) => match serde_json::from_str::<serde_json::Value>(&resp_body) {
                        Ok(resp) => {
                            if resp.get("error").and_then(|v| v.as_str()).is_some() {
                                Err(CliError::not_found("run", &run_id))
                            } else {
                                Ok(resp)
                            }
                        }
                        Err(e) => Err(CliError::parse_error(&e.to_string())),
                    },
                    Err(_) => Err(CliError::daemon_unreachable(&cli.addr)),
                };
                output_result(result, cli.json, |data| {
                    if let Some(status) = data.get("status").and_then(|v| v.as_str()) {
                        println!("{}", status);
                    }
                });
            }
        },
        Some(Commands::AgentHelp) => {
            let manifest = build_manifest();
            println!("{}", serde_json::to_string_pretty(&manifest).unwrap());
        }
        Some(Commands::Worker { agent, config }) => {
            zeptopm::worker::run(agent, config).await;
        }
        Some(Commands::Logs { name }) => {
            let result: CliResult =
                match http_get(&cli.addr, &format!("/agents/{}/logs", name)).await {
                    Ok(body) => match serde_json::from_str::<serde_json::Value>(&body) {
                        Ok(resp) => {
                            if resp.get("error").and_then(|v| v.as_str()).is_some() {
                                Err(CliError::not_found("agent", &name))
                            } else {
                                Ok(resp)
                            }
                        }
                        Err(e) => Err(CliError::parse_error(&e.to_string())),
                    },
                    Err(_) => Err(CliError::daemon_unreachable(&cli.addr)),
                };
            output_result(result, cli.json, |data| {
                if let Some(logs) = data.get("logs").and_then(|v| v.as_array()) {
                    if logs.is_empty() {
                        println!("No logs for agent '{}'.", name);
                        return;
                    }
                    for entry in logs {
                        let ts = entry
                            .get("timestamp")
                            .and_then(|v| v.as_str())
                            .unwrap_or("?");
                        let level = entry.get("level").and_then(|v| v.as_str()).unwrap_or("?");
                        let msg = entry.get("message").and_then(|v| v.as_str()).unwrap_or("?");
                        println!("{} [{}] {}", ts, level, msg);
                    }
                }
            });
        }
        None => {
            // Default: run daemon
            let config = zeptopm::config::load_config(&cli.config).unwrap_or_else(|e| {
                eprintln!("Failed to load config: {}", e);
                std::process::exit(1);
            });

            let log_level = cli.log_level.as_deref().unwrap_or(&config.daemon.log_level);
            init_tracing(log_level, &config.daemon.log_format);

            zeptopm::daemon::run(cli.config, None).await;
        }
    }
}

/// Print a formatted run status snapshot.
fn print_run_status(body: &str) {
    let resp: serde_json::Value = match serde_json::from_str(body) {
        Ok(v) => v,
        Err(_) => {
            println!("{}", body);
            return;
        }
    };
    if let Some(error) = resp.get("error").and_then(|v| v.as_str()) {
        eprintln!("Error: {}", error);
        return;
    }

    let status = resp.get("status").and_then(|v| v.as_str()).unwrap_or("?");
    let task = resp.get("task").and_then(|v| v.as_str()).unwrap_or("?");
    let run_id = resp.get("run_id").and_then(|v| v.as_str()).unwrap_or("?");

    println!("Run: {}  Status: {}", run_id, status);
    println!("Task: {}", task);

    if let Some(jobs) = resp.get("jobs").and_then(|v| v.as_array()) {
        if !jobs.is_empty() {
            println!(
                "\n{:<24} {:<12} {:<12} {}",
                "JOB ID", "ROLE", "STATUS", "INSTRUCTION"
            );
            println!("{}", "-".repeat(80));
            for job in jobs {
                let jid = job.get("job_id").and_then(|v| v.as_str()).unwrap_or("?");
                let role = job.get("role").and_then(|v| v.as_str()).unwrap_or("?");
                let st = job.get("status").and_then(|v| v.as_str()).unwrap_or("?");
                let instr = job
                    .get("instruction")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let short_instr: String = instr.chars().take(40).collect();
                println!("{:<24} {:<12} {:<12} {}", jid, role, st, short_instr);
            }
        }
    }
}

/// Follow a run's progress in real-time by polling.
async fn tail_run(addr: &str, run_id: &str) {
    use std::collections::HashMap;

    println!("\nFollowing run {}... (Ctrl+C to stop)\n", run_id);

    let mut seen_statuses: HashMap<String, String> = HashMap::new();
    let mut last_run_status = String::new();

    loop {
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;

        let body = match http_get(addr, &format!("/runs/{}", run_id)).await {
            Ok(b) => b,
            Err(e) => {
                eprintln!("  [poll error: {}]", e);
                continue;
            }
        };

        let resp: serde_json::Value = match serde_json::from_str(&body) {
            Ok(v) => v,
            Err(_) => continue,
        };

        let run_status = resp
            .get("status")
            .and_then(|v| v.as_str())
            .unwrap_or("?")
            .to_string();

        // Check for job-level changes
        if let Some(jobs) = resp.get("jobs").and_then(|v| v.as_array()) {
            for job in jobs {
                let jid = job
                    .get("job_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("?")
                    .to_string();
                let role = job.get("role").and_then(|v| v.as_str()).unwrap_or("?");
                let st = job
                    .get("status")
                    .and_then(|v| v.as_str())
                    .unwrap_or("?")
                    .to_string();
                let prev = seen_statuses.get(&jid).map(|s| s.as_str()).unwrap_or("");

                if prev != st {
                    let now = chrono::Local::now().format("%H:%M:%S");
                    match st.as_str() {
                        "Running" => println!("  {} [{}] {} starting...", now, role, jid),
                        "Completed" => println!("  {} [{}] {} completed", now, role, jid),
                        "Failed" => {
                            let err = job
                                .get("error")
                                .and_then(|v| v.as_str())
                                .unwrap_or("unknown");
                            println!("  {} [{}] {} FAILED: {}", now, role, jid, err);
                        }
                        "Ready" => println!("  {} [{}] {} ready (queued)", now, role, jid),
                        _ => println!("  {} [{}] {} -> {}", now, role, jid, st),
                    }
                    seen_statuses.insert(jid, st);
                }
            }
        }

        // Check run-level status change
        if run_status != last_run_status {
            if run_status == "Completed" || run_status == "Failed" || run_status == "Cancelled" {
                let now = chrono::Local::now().format("%H:%M:%S");
                println!("\n  {} Run {} -> {}", now, run_id, run_status);
                break;
            }
            last_run_status = run_status;
        }
    }
}

async fn http_get(addr: &str, path: &str) -> Result<String, String> {
    let url = format!("http://{}{}", addr, path);
    let resp = reqwest::get(&url).await.map_err(|e| e.to_string())?;
    resp.text().await.map_err(|e| e.to_string())
}

async fn http_post(addr: &str, path: &str, body: &serde_json::Value) -> Result<String, String> {
    let url = format!("http://{}{}", addr, path);
    let client = reqwest::Client::new();
    let resp = client
        .post(&url)
        .json(body)
        .send()
        .await
        .map_err(|e| e.to_string())?;
    resp.text().await.map_err(|e| e.to_string())
}

/// Build the full CLI manifest for agent discovery.
fn build_manifest() -> serde_json::Value {
    serde_json::json!({
        "version": env!("CARGO_PKG_VERSION"),
        "commands": {
            "status": {
                "description": "Show status of all running agents (queries daemon)",
                "args": [],
                "flags": ["--json"],
                "output_shape": {
                    "agents": [{"name": "str", "status": "str", "restart_count": "int", "tokens_used": "int", "uptime_secs": "int"}]
                }
            },
            "list": {
                "description": "List configured agents from config file (no daemon needed)",
                "args": [],
                "flags": ["--json"],
                "output_shape": {
                    "agents": [{"name": "str", "auto_start": "bool", "provider": "str", "model": "str"}]
                }
            },
            "chat": {
                "description": "Send a message to an agent and get the response",
                "args": [
                    {"name": "name", "type": "string", "required": true, "description": "Agent name"},
                    {"name": "message", "type": "string", "required": true, "description": "Message to send"}
                ],
                "flags": ["--json"],
                "output_shape": {"response": "str"}
            },
            "stop": {
                "description": "Stop a running agent",
                "args": [{"name": "name", "type": "string", "required": true, "description": "Agent name"}],
                "flags": ["--json"],
                "output_shape": {"status": "str"}
            },
            "start": {
                "description": "Start an agent (must be defined in config)",
                "args": [{"name": "name", "type": "string", "required": true, "description": "Agent name"}],
                "flags": ["--json"],
                "output_shape": {"status": "str"}
            },
            "restart": {
                "description": "Restart an agent (stop + start)",
                "args": [{"name": "name", "type": "string", "required": true, "description": "Agent name"}],
                "flags": ["--json"],
                "output_shape": {"status": "str"}
            },
            "logs": {
                "description": "Show recent logs for an agent",
                "args": [{"name": "name", "type": "string", "required": true, "description": "Agent name"}],
                "flags": ["--json"],
                "output_shape": {
                    "logs": [{"timestamp": "str", "level": "str", "message": "str"}]
                }
            },
            "pipeline": {
                "description": "Chain agents in a pipeline (output of one feeds into the next)",
                "args": [
                    {"name": "agents", "type": "string", "required": true, "description": "Comma-separated agent names"},
                    {"name": "message", "type": "string", "required": true, "description": "Message to start the pipeline"}
                ],
                "flags": ["--json"],
                "output_shape": {
                    "steps": [{"agent": "str", "response": "str"}]
                }
            },
            "orchestrate": {
                "description": "Orchestrate multi-agent collaboration (manager delegates to other agents)",
                "args": [
                    {"name": "manager", "type": "string", "required": true, "description": "Manager agent name"},
                    {"name": "message", "type": "string", "required": true, "description": "Task for the manager"}
                ],
                "flags": ["--json"],
                "output_shape": {
                    "response": "str",
                    "delegations": [{"to": "str", "query": "str", "result": "str"}],
                    "rounds": "int"
                }
            },
            "run submit": {
                "description": "Submit a new orchestrated run",
                "args": [{"name": "task", "type": "string", "required": true, "description": "Task description"}],
                "flags": ["--tail", "--json"],
                "output_shape": {"run_id": "str"}
            },
            "run status": {
                "description": "Check status of a run",
                "args": [{"name": "run_id", "type": "string", "required": true, "description": "Run ID"}],
                "flags": ["--tail", "--json"],
                "output_shape": {
                    "run_id": "str", "status": "str", "task": "str",
                    "jobs": [{"job_id": "str", "role": "str", "status": "str", "instruction": "str"}]
                }
            },
            "run list": {
                "description": "List all runs",
                "args": [],
                "flags": ["--json"],
                "output_shape": {
                    "runs": [{"run_id": "str", "status": "str", "task": "str"}]
                }
            },
            "run result": {
                "description": "Print final artifact content for a completed run",
                "args": [{"name": "run_id", "type": "string", "required": true, "description": "Run ID"}],
                "flags": ["--json"],
                "output_shape": {
                    "status": "str",
                    "artifacts": [{"kind": "str", "summary": "str", "path": "str", "content": "str"}]
                }
            },
            "run cancel": {
                "description": "Cancel a running run (cancels all active jobs)",
                "args": [{"name": "run_id", "type": "string", "required": true, "description": "Run ID"}],
                "flags": ["--json"],
                "output_shape": {"status": "str"}
            }
        },
        "workflows": {
            "submit_and_wait": {
                "description": "Submit a run and poll until completion, then get results",
                "steps": [
                    "zeptopm run submit \"<task>\" --json  →  {\"ok\": true, \"data\": {\"run_id\": \"...\"}}",
                    "zeptopm run status <run_id> --json  →  poll until data.status is \"Completed\", \"Failed\", or \"Cancelled\"",
                    "zeptopm run result <run_id> --json  →  {\"ok\": true, \"data\": {\"artifacts\": [...]}}"
                ]
            },
            "agent_chat": {
                "description": "Find an available agent and chat with it",
                "steps": [
                    "zeptopm status --json  →  find agents where status is \"running\"",
                    "zeptopm chat <name> \"<message>\" --json  →  {\"ok\": true, \"data\": {\"response\": \"...\"}}"
                ]
            },
            "monitor_agents": {
                "description": "Check agent health and investigate issues",
                "steps": [
                    "zeptopm status --json  →  check all agent health (restarts, uptime)",
                    "zeptopm logs <name> --json  →  get recent log entries for a specific agent"
                ]
            }
        },
        "error_codes": {
            "DAEMON_UNREACHABLE": "Daemon is not running or address is wrong. Start with: zeptopm daemon",
            "RUN_NOT_FOUND": "No run with this ID exists",
            "AGENT_NOT_FOUND": "No agent with this name in config or not running",
            "INVALID_CONFIG": "Config file is missing or has validation errors",
            "PARSE_ERROR": "Failed to parse response from daemon"
        }
    })
}

/// Structured CLI error for JSON output mode.
struct CliError {
    message: String,
    code: String,
}

impl CliError {
    fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "ok": false,
            "error": self.message,
            "code": self.code,
        })
    }

    fn daemon_unreachable(addr: &str) -> Self {
        CliError {
            message: format!("Failed to connect to daemon at {}", addr),
            code: "DAEMON_UNREACHABLE".into(),
        }
    }

    fn parse_error(detail: &str) -> Self {
        CliError {
            message: format!("Failed to parse response: {}", detail),
            code: "PARSE_ERROR".into(),
        }
    }

    fn not_found(kind: &str, id: &str) -> Self {
        let code = match kind {
            "run" => "RUN_NOT_FOUND",
            "agent" => "AGENT_NOT_FOUND",
            _ => "NOT_FOUND",
        };
        CliError {
            message: format!("{} '{}' not found", kind, id),
            code: code.into(),
        }
    }

    fn invalid_config(detail: &str) -> Self {
        CliError {
            message: format!("Invalid config: {}", detail),
            code: "INVALID_CONFIG".into(),
        }
    }
}

type CliResult = Result<serde_json::Value, CliError>;

/// Format a CliResult as a JSON envelope string.
fn format_output_json(result: &CliResult) -> String {
    match result {
        Ok(data) => {
            let envelope = serde_json::json!({ "ok": true, "data": data });
            serde_json::to_string_pretty(&envelope).unwrap()
        }
        Err(err) => serde_json::to_string_pretty(&err.to_json()).unwrap(),
    }
}

/// Output a CliResult — JSON envelope if json_mode, otherwise run the human formatter.
fn output_result(result: CliResult, json_mode: bool, human_fn: impl FnOnce(&serde_json::Value)) {
    match (json_mode, &result) {
        (true, _) => {
            println!("{}", format_output_json(&result));
            if result.is_err() {
                std::process::exit(1);
            }
        }
        (false, Ok(data)) => human_fn(data),
        (false, Err(err)) => {
            eprintln!("Error: {}", err.message);
            std::process::exit(1);
        }
    }
}

fn format_uptime(secs: u64) -> String {
    if secs < 60 {
        format!("{}s", secs)
    } else if secs < 3600 {
        format!("{}m {}s", secs / 60, secs % 60)
    } else {
        format!("{}h {}m", secs / 3600, (secs % 3600) / 60)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cli_error_to_json() {
        let err = CliError {
            message: "Run not found".into(),
            code: "RUN_NOT_FOUND".into(),
        };
        let json = err.to_json();
        assert_eq!(json["ok"], false);
        assert_eq!(json["error"], "Run not found");
        assert_eq!(json["code"], "RUN_NOT_FOUND");
    }

    #[test]
    fn test_format_success_json() {
        let data = serde_json::json!({"run_id": "run_123"});
        let output = format_output_json(&Ok(data.clone()));
        let parsed: serde_json::Value = serde_json::from_str(&output).unwrap();
        assert_eq!(parsed["ok"], true);
        assert_eq!(parsed["data"]["run_id"], "run_123");
    }

    #[test]
    fn test_format_error_json() {
        let err = CliError {
            message: "Daemon unreachable".into(),
            code: "DAEMON_UNREACHABLE".into(),
        };
        let output = format_output_json(&Err(err));
        let parsed: serde_json::Value = serde_json::from_str(&output).unwrap();
        assert_eq!(parsed["ok"], false);
        assert_eq!(parsed["error"], "Daemon unreachable");
        assert_eq!(parsed["code"], "DAEMON_UNREACHABLE");
    }

    #[test]
    fn test_manifest_has_all_commands() {
        let manifest = build_manifest();
        let commands = manifest["commands"].as_object().unwrap();
        let expected = vec![
            "status",
            "list",
            "chat",
            "logs",
            "stop",
            "start",
            "restart",
            "pipeline",
            "orchestrate",
            "run submit",
            "run status",
            "run list",
            "run result",
            "run cancel",
        ];
        for cmd in &expected {
            assert!(commands.contains_key(*cmd), "missing command: {}", cmd);
        }
        for (name, spec) in commands {
            assert!(
                spec.get("description").is_some(),
                "{} missing description",
                name
            );
            assert!(
                spec.get("output_shape").is_some(),
                "{} missing output_shape",
                name
            );
        }
    }

    #[test]
    fn test_manifest_has_workflows() {
        let manifest = build_manifest();
        let workflows = manifest["workflows"].as_object().unwrap();
        assert!(workflows.contains_key("submit_and_wait"));
        assert!(workflows.contains_key("agent_chat"));
        assert!(workflows.contains_key("monitor_agents"));
        for (name, wf) in workflows {
            assert!(
                wf.get("steps").and_then(|s| s.as_array()).is_some(),
                "{} missing steps",
                name
            );
        }
    }

    #[test]
    fn test_manifest_has_error_codes() {
        let manifest = build_manifest();
        let codes = manifest["error_codes"].as_object().unwrap();
        assert!(codes.contains_key("DAEMON_UNREACHABLE"));
        assert!(codes.contains_key("RUN_NOT_FOUND"));
        assert!(codes.contains_key("AGENT_NOT_FOUND"));
        assert!(codes.contains_key("INVALID_CONFIG"));
        assert!(codes.contains_key("PARSE_ERROR"));
    }

    #[test]
    fn test_manifest_version_present() {
        let manifest = build_manifest();
        assert!(manifest.get("version").and_then(|v| v.as_str()).is_some());
    }
}
