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
      tracing_subscriber::fmt()
        .with_env_filter(env_filter)
        .init();
    }
  }
}

#[tokio::main]
async fn main() {
  let cli = Cli::parse();

  match cli.command {
    Some(Commands::Daemon { bind }) => {
      // Load config to get log settings
      let config = zeptopm::config::load_config(&cli.config).unwrap_or_else(|e| {
        eprintln!("Failed to load config: {}", e);
        std::process::exit(1);
      });

      let log_level = cli
        .log_level
        .as_deref()
        .unwrap_or(&config.daemon.log_level);
      init_tracing(log_level, &config.daemon.log_format);

      zeptopm::daemon::run(cli.config, bind).await;
    }
    Some(Commands::Status) => {
      match http_get(&cli.addr, "/status").await {
        Ok(body) => {
          let resp: serde_json::Value = serde_json::from_str(&body).unwrap_or_default();
          if let Some(agents) = resp.get("agents").and_then(|a| a.as_array()) {
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
              let restarts = agent.get("restart_count").and_then(|v| v.as_u64()).unwrap_or(0);
              let tokens = agent.get("tokens_used").and_then(|v| v.as_u64()).unwrap_or(0);
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
        }
        Err(e) => {
          eprintln!("Failed to connect to daemon at {}: {}", cli.addr, e);
          eprintln!("Is the daemon running? Start with: zeptopm daemon");
          std::process::exit(1);
        }
      }
    }
    Some(Commands::List) => {
      let config = match zeptopm::config::load_config(&cli.config) {
        Ok(c) => c,
        Err(e) => {
          eprintln!("Failed to load config: {}", e);
          std::process::exit(1);
        }
      };
      for agent in &config.agents {
        let auto = if agent.auto_start { "auto" } else { "manual" };
        let model = agent.model.as_deref().unwrap_or("default");
        println!(
          "{:<20} {:<10} provider={:<15} model={}",
          agent.name, auto, agent.provider, model
        );
      }
    }
    Some(Commands::Chat { name, message }) => {
      let body = serde_json::json!({ "message": message });
      match http_post(&cli.addr, &format!("/agents/{}/chat", name), &body).await {
        Ok(resp_body) => {
          let resp: serde_json::Value = serde_json::from_str(&resp_body).unwrap_or_default();
          if let Some(error) = resp.get("error").and_then(|v| v.as_str()) {
            eprintln!("Error: {}", error);
            std::process::exit(1);
          }
          if let Some(content) = resp.get("response").and_then(|v| v.as_str()) {
            println!("{}", content);
          }
        }
        Err(e) => {
          eprintln!("Failed to connect to daemon at {}: {}", cli.addr, e);
          eprintln!("Is the daemon running? Start with: zeptopm daemon");
          std::process::exit(1);
        }
      }
    }
    Some(Commands::Stop { name }) => {
      match http_post(
        &cli.addr,
        &format!("/agents/{}/stop", name),
        &serde_json::json!({}),
      )
      .await
      {
        Ok(resp_body) => {
          let resp: serde_json::Value = serde_json::from_str(&resp_body).unwrap_or_default();
          if let Some(error) = resp.get("error").and_then(|v| v.as_str()) {
            eprintln!("Error: {}", error);
            std::process::exit(1);
          }
          if let Some(status) = resp.get("status").and_then(|v| v.as_str()) {
            println!("{}", status);
          }
        }
        Err(e) => {
          eprintln!("Failed to connect to daemon at {}: {}", cli.addr, e);
          std::process::exit(1);
        }
      }
    }
    Some(Commands::Start { name }) => {
      match http_post(
        &cli.addr,
        &format!("/agents/{}/start", name),
        &serde_json::json!({}),
      )
      .await
      {
        Ok(resp_body) => {
          let resp: serde_json::Value = serde_json::from_str(&resp_body).unwrap_or_default();
          if let Some(error) = resp.get("error").and_then(|v| v.as_str()) {
            eprintln!("Error: {}", error);
            std::process::exit(1);
          }
          if let Some(status) = resp.get("status").and_then(|v| v.as_str()) {
            println!("{}", status);
          }
        }
        Err(e) => {
          eprintln!("Failed to connect to daemon at {}: {}", cli.addr, e);
          std::process::exit(1);
        }
      }
    }
    Some(Commands::Restart { name }) => {
      match http_post(
        &cli.addr,
        &format!("/agents/{}/restart", name),
        &serde_json::json!({}),
      )
      .await
      {
        Ok(resp_body) => {
          let resp: serde_json::Value = serde_json::from_str(&resp_body).unwrap_or_default();
          if let Some(error) = resp.get("error").and_then(|v| v.as_str()) {
            eprintln!("Error: {}", error);
            std::process::exit(1);
          }
          if let Some(status) = resp.get("status").and_then(|v| v.as_str()) {
            println!("{}", status);
          }
        }
        Err(e) => {
          eprintln!("Failed to connect to daemon at {}: {}", cli.addr, e);
          std::process::exit(1);
        }
      }
    }
    Some(Commands::Logs { name }) => {
      match http_get(&cli.addr, &format!("/agents/{}/logs", name)).await {
        Ok(body) => {
          let resp: serde_json::Value = serde_json::from_str(&body).unwrap_or_default();
          if let Some(error) = resp.get("error").and_then(|v| v.as_str()) {
            eprintln!("Error: {}", error);
            std::process::exit(1);
          }
          if let Some(logs) = resp.get("logs").and_then(|v| v.as_array()) {
            if logs.is_empty() {
              println!("No logs for agent '{}'.", name);
              return;
            }
            for entry in logs {
              let ts = entry.get("timestamp").and_then(|v| v.as_str()).unwrap_or("?");
              let level = entry.get("level").and_then(|v| v.as_str()).unwrap_or("?");
              let msg = entry.get("message").and_then(|v| v.as_str()).unwrap_or("?");
              println!("{} [{}] {}", ts, level, msg);
            }
          }
        }
        Err(e) => {
          eprintln!("Failed to connect to daemon at {}: {}", cli.addr, e);
          eprintln!("Is the daemon running? Start with: zeptopm daemon");
          std::process::exit(1);
        }
      }
    }
    None => {
      // Default: run daemon
      let config = zeptopm::config::load_config(&cli.config).unwrap_or_else(|e| {
        eprintln!("Failed to load config: {}", e);
        std::process::exit(1);
      });

      let log_level = cli
        .log_level
        .as_deref()
        .unwrap_or(&config.daemon.log_level);
      init_tracing(log_level, &config.daemon.log_format);

      zeptopm::daemon::run(cli.config, None).await;
    }
  }
}

async fn http_get(addr: &str, path: &str) -> Result<String, String> {
  let url = format!("http://{}{}", addr, path);
  let resp = reqwest::get(&url)
    .await
    .map_err(|e| e.to_string())?;
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

fn format_uptime(secs: u64) -> String {
  if secs < 60 {
    format!("{}s", secs)
  } else if secs < 3600 {
    format!("{}m {}s", secs / 60, secs % 60)
  } else {
    format!("{}h {}m", secs / 3600, (secs % 3600) / 60)
  }
}
