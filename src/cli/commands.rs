use crate::config::loader::find_config_file;
use crate::config::model::ToolType;
use crate::config::{load_config, validate};
use crate::mcp;
use crate::observability;
use crate::runtime::app_state::AppState;
use crate::runtime::registry::ToolRegistry;
use crate::supervisor::manager::SupervisorManager;
use anyhow::{Context, Result};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

const INIT_TEMPLATE: &str = r#"server:
  name: my-project
  transport: stdio
  log_level: info

tools:
  - name: hello
    type: exec
    description: Say hello
    command: echo
    args: ["Hello from mcpify!"]
    timeout_ms: 5000
    input:
      type: object
      properties: {}
      required: []
"#;

const PID_DIR: &str = ".mcpify";
const PID_FILE: &str = ".mcpify/mcpify.pid";

pub async fn cmd_init() -> Result<()> {
    let path = Path::new("mcpify.yaml");
    if path.exists() {
        anyhow::bail!("mcpify.yaml already exists");
    }
    std::fs::write(path, INIT_TEMPLATE)?;
    println!("Created mcpify.yaml");
    Ok(())
}

pub async fn cmd_validate(config_path: Option<&Path>) -> Result<()> {
    let config = load_config(config_path)?;
    let warnings = validate(&config)?;
    for w in &warnings {
        eprintln!("warning: {}", w.message);
    }
    println!(
        "Config is valid ({} tools, {} services)",
        config.tools.len(),
        config.services.len()
    );
    Ok(())
}

pub async fn cmd_list(config_path: Option<&Path>) -> Result<()> {
    let config = load_config(config_path)?;
    let registry = ToolRegistry::from_config(&config);

    println!("{:<20} {:<6} {:<40} TIMEOUT", "NAME", "TYPE", "DESCRIPTION");
    println!("{}", "-".repeat(80));
    for entry in registry.list() {
        let t = &entry.config;
        let type_str = match t.tool_type {
            ToolType::Exec => "exec",
            ToolType::Http => "http",
        };
        println!(
            "{:<20} {:<6} {:<40} {}ms",
            t.name, type_str, t.description, t.timeout_ms
        );
    }
    Ok(())
}

pub async fn cmd_run(config_path: Option<&Path>, tool_name: &str, input_json: &str) -> Result<()> {
    let config = load_config(config_path)?;
    validate(&config)?;
    let registry = ToolRegistry::from_config(&config);

    let entry = registry
        .get(tool_name)
        .ok_or_else(|| crate::errors::McpifyError::ToolNotFound(tool_name.to_string()))?;

    let input: serde_json::Value =
        serde_json::from_str(input_json).context("parsing --input JSON")?;

    let result = match entry.config.tool_type {
        ToolType::Exec => crate::adapters::exec::execute(&entry.config, input).await?,
        ToolType::Http => {
            let client = reqwest::Client::new();
            crate::adapters::http::execute(&entry.config, input, &client).await?
        }
    };

    if !result.stdout.is_empty() {
        print!("{}", result.stdout);
    }
    if !result.stderr.is_empty() {
        eprint!("{}", result.stderr);
    }
    if result.is_error {
        std::process::exit(result.exit_code.unwrap_or(1));
    }

    Ok(())
}

pub async fn cmd_serve(config_path: Option<&Path>, watch: bool) -> Result<()> {
    let config_path_buf = match config_path {
        Some(p) => p.to_path_buf(),
        None => find_config_file()?,
    };

    let config = load_config(Some(&config_path_buf))?;
    validate(&config)?;

    observability::init_logging(&config.server.log_level);
    tracing::info!(
        name = %config.server.name,
        tools = config.tools.len(),
        services = config.services.len(),
        "starting mcpify server"
    );

    let healthcheck_interval = config.supervisor.healthcheck_interval_ms;
    let registry = ToolRegistry::from_config(&config);
    let mut supervisor = SupervisorManager::from_config(&config);

    // Start services with autostart=true
    supervisor.start_all().await?;

    let state = Arc::new(AppState::new(config, registry, supervisor));

    // Write PID file
    write_pid_file()?;

    // Spawn health check loop
    let health_state = Arc::clone(&state);
    tokio::spawn(async move {
        let interval = Duration::from_millis(healthcheck_interval);
        loop {
            tokio::time::sleep(interval).await;
            let mut sup = health_state.supervisor.write().await;
            sup.run_health_checks().await;
            sup.handle_restarts().await;
        }
    });

    // Spawn SIGHUP reload handler
    #[cfg(unix)]
    crate::runtime::reload::spawn_signal_handler(Arc::clone(&state), config_path_buf.clone());

    // Spawn file watcher if --watch
    if watch {
        crate::runtime::reload::spawn_file_watcher(Arc::clone(&state), config_path_buf);
    }

    // Run MCP server (blocks until client disconnects)
    let serve_result = mcp::run_stdio_server(Arc::clone(&state)).await;

    // Cleanup
    tracing::info!("shutting down services");
    let mut sup = state.supervisor.write().await;
    sup.stop_all().await?;
    remove_pid_file();

    serve_result
}

pub async fn cmd_reload() -> Result<()> {
    let pid_path = Path::new(PID_FILE);
    if !pid_path.exists() {
        anyhow::bail!("no running mcpify server found (PID file not found at {PID_FILE})");
    }

    let pid_str = std::fs::read_to_string(pid_path).context("reading PID file")?;
    let pid: i32 = pid_str.trim().parse().context("parsing PID from file")?;

    #[cfg(unix)]
    {
        use nix::sys::signal::{Signal, kill};
        use nix::unistd::Pid;

        kill(Pid::from_raw(pid), Signal::SIGHUP).with_context(|| {
            format!("failed to send SIGHUP to PID {pid} — process may not be running")
        })?;
        println!("Sent reload signal to mcpify (PID {pid})");
    }

    #[cfg(not(unix))]
    {
        let _ = pid;
        anyhow::bail!("reload via signal is only supported on Unix");
    }

    Ok(())
}

pub async fn cmd_status(config_path: Option<&Path>) -> Result<()> {
    let config = load_config(config_path)?;
    let registry = ToolRegistry::from_config(&config);

    println!("=== Tools ===");
    println!("{:<20} {:<6} {:<12} DEPENDS_ON", "NAME", "TYPE", "STATUS");
    println!("{}", "-".repeat(60));
    for entry in registry.list() {
        let t = &entry.config;
        let type_str = match t.tool_type {
            ToolType::Exec => "exec",
            ToolType::Http => "http",
        };
        let deps = if t.depends_on.is_empty() {
            "-".to_string()
        } else {
            t.depends_on.join(", ")
        };
        println!(
            "{:<20} {:<6} {:<12} {}",
            t.name,
            type_str,
            format!("{:?}", entry.availability),
            deps
        );
    }

    if !config.services.is_empty() {
        println!("\n=== Services ===");
        println!(
            "{:<20} {:<30} {:<10} HEALTHCHECK",
            "NAME", "COMMAND", "AUTOSTART"
        );
        println!("{}", "-".repeat(70));
        for svc in &config.services {
            let cmd = format!("{} {}", svc.command, svc.args.join(" "));
            let hc = match &svc.healthcheck {
                Some(h) => format!("{:?}", h.check_type),
                None => "process".to_string(),
            };
            println!("{:<20} {:<30} {:<10} {}", svc.name, cmd, svc.autostart, hc);
        }
    }

    // Show if server is running
    let pid_path = Path::new(PID_FILE);
    if pid_path.exists()
        && let Ok(pid) = std::fs::read_to_string(pid_path)
    {
        println!("\nServer PID: {}", pid.trim());
    }

    Ok(())
}

fn write_pid_file() -> Result<()> {
    std::fs::create_dir_all(PID_DIR)?;
    let pid = std::process::id();
    std::fs::write(PID_FILE, pid.to_string())?;
    tracing::debug!(pid, "wrote PID file");
    Ok(())
}

fn remove_pid_file() {
    let _ = std::fs::remove_file(PID_FILE);
}
