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
}

#[derive(clap::Subcommand, Debug)]
enum Commands {
  /// Start the daemon — runs all auto_start agents
  Daemon {
    /// Override server bind address
    #[arg(short, long)]
    bind: Option<String>,
  },
  /// Show status of all agents
  Status,
  /// List configured agents
  List,
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
      let config = match zeptopm::config::load_config(&cli.config) {
        Ok(c) => c,
        Err(e) => {
          eprintln!("Failed to load config: {}", e);
          std::process::exit(1);
        }
      };
      // Simple status: just list configured agents
      println!("Configured agents ({}):", config.agents.len());
      for agent in &config.agents {
        let auto = if agent.auto_start { "auto" } else { "manual" };
        let model = agent.model.as_deref().unwrap_or("default");
        println!(
          "  {} [{}] provider={} model={} {}",
          agent.name,
          auto,
          agent.provider,
          model,
          if agent.auto_start { "" } else { "(not auto-started)" }
        );
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
        println!("{}", agent.name);
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
