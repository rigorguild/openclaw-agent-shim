// agent-shim — minimal HTTP-to-CLI forwarder for openclaw agent invocations.
//
// Workaround for known bugs in openclaw 2026.4.x around webhook-driven agent
// dispatch via hooks.mappings — agentId / sessionKey are silently ignored and
// the configured messageTemplate is dropped, so the agent ends up running in
// the wrong session with the wrong prompt. See:
//   https://github.com/openclaw/openclaw/issues/64556
//   https://github.com/openclaw/openclaw/issues/70894
//
// Pure pass-through: receives POST /<agent_id> with an arbitrary body, validates
// auth, and spawns `openclaw agent --agent <id> -m <body> --json` capturing
// output to a per-run file. No template rendering, no prompt logic. All
// semantics live in the agent's CLAUDE.md inside its openclaw workspace.
//
// Endpoints:
//   POST /<agent_id>     dispatch a run, returns {runId} immediately
//   GET  /runs/<runId>   read run state (running | completed openclaw output)

use std::collections::HashSet;
use std::env;
use std::fs;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::Command;
use std::sync::OnceLock;
use std::thread;

use tiny_http::{Header, Method, Response, Server};

struct Config {
    bind: String,
    openclaw_bin: String,
    runs_dir: PathBuf,
    openclaw_config: PathBuf,
    token: String,
}

impl Config {
    fn from_env() -> Self {
        let bind = env::var("AGENT_SHIM_BIND")
            .unwrap_or_else(|_| "127.0.0.1:18790".to_string());
        let openclaw_bin = env::var("AGENT_SHIM_OPENCLAW_BIN")
            .unwrap_or_else(|_| "openclaw".to_string());
        let runs_dir = env::var("AGENT_SHIM_RUNS_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("/var/tmp/agent-shim/runs"));
        let openclaw_config = env::var("AGENT_SHIM_OPENCLAW_CONFIG")
            .map(PathBuf::from)
            .unwrap_or_else(|_| {
                let home = env::var("HOME").unwrap_or_default();
                PathBuf::from(home).join(".openclaw/openclaw.json")
            });
        let token = env::var("AGENT_SHIM_TOKEN").unwrap_or_default();
        Self {
            bind,
            openclaw_bin,
            runs_dir,
            openclaw_config,
            token,
        }
    }
}

static CONFIG: OnceLock<Config> = OnceLock::new();

fn config() -> &'static Config {
    CONFIG.get().expect("Config must be initialized in main()")
}

/// Read the openclaw config and return the set of registered agent IDs.
/// On any error reading/parsing, returns None — caller decides how to handle.
fn load_known_agent_ids() -> Option<HashSet<String>> {
    let content = fs::read_to_string(&config().openclaw_config).ok()?;
    let parsed: serde_json::Value = serde_json::from_str(&content).ok()?;
    let list = parsed.get("agents")?.get("list")?.as_array()?;
    Some(
        list.iter()
            .filter_map(|e| e.get("id").and_then(|i| i.as_str()).map(String::from))
            .collect(),
    )
}

fn is_valid_agent_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
}

fn is_valid_run_id(id: &str) -> bool {
    !id.is_empty() && id.len() == 32 && id.chars().all(|c| c.is_ascii_hexdigit())
}

fn gen_run_id() -> std::io::Result<String> {
    let mut bytes = [0u8; 16];
    fs::File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    Ok(bytes.iter().map(|b| format!("{:02x}", b)).collect())
}

fn run_state_path(run_id: &str) -> PathBuf {
    config().runs_dir.join(format!("{}.json", run_id))
}

fn dispatch_agent_run(agent_id: String, message: String, run_id: String) {
    thread::spawn(move || {
        let final_path = run_state_path(&run_id);
        let tmp_path = final_path.with_extension("json.tmp");

        // Reuse run_id (32 hex chars) as a unique session-id per webhook so
        // each invocation gets a fresh claude-cli session — prevents history
        // accumulation and language drift from prior turns.
        let session_id = format!(
            "{}-{}-{}-{}-{}",
            &run_id[0..8],
            &run_id[8..12],
            &run_id[12..16],
            &run_id[16..20],
            &run_id[20..32],
        );

        let output = Command::new(&config().openclaw_bin)
            .arg("agent")
            .arg("--agent")
            .arg(&agent_id)
            .arg("--session-id")
            .arg(&session_id)
            .arg("-m")
            .arg(&message)
            .arg("--json")
            .output();

        let body = match output {
            Ok(o) => {
                let stdout_str = String::from_utf8_lossy(&o.stdout).to_string();
                let stderr_str = String::from_utf8_lossy(&o.stderr).to_string();
                let parsed: serde_json::Value = serde_json::from_str(&stdout_str)
                    .unwrap_or(serde_json::Value::Null);
                serde_json::json!({
                    "shimRunId": run_id,
                    "agentId": agent_id,
                    "exitCode": o.status.code(),
                    "openclaw": parsed,
                    "stderr": stderr_str,
                })
            }
            Err(e) => serde_json::json!({
                "shimRunId": run_id,
                "agentId": agent_id,
                "error": format!("spawn failed: {}", e),
            }),
        };

        let serialized = body.to_string();
        if let Err(e) = (|| -> std::io::Result<()> {
            let mut f = fs::File::create(&tmp_path)?;
            f.write_all(serialized.as_bytes())?;
            f.sync_all()?;
            fs::rename(&tmp_path, &final_path)?;
            Ok(())
        })() {
            eprintln!(
                "agent-shim: failed to write run state for {}: {}",
                run_id, e
            );
        }
    });
}

fn extract_bearer(headers: &[Header]) -> Option<String> {
    for h in headers {
        if h.field.as_str().as_str().eq_ignore_ascii_case("authorization") {
            let v = h.value.as_str();
            if let Some(rest) = v.strip_prefix("Bearer ") {
                return Some(rest.to_string());
            }
        }
    }
    None
}

fn respond_text(request: tiny_http::Request, code: u16, body: &str) -> std::io::Result<()> {
    request.respond(Response::from_string(body).with_status_code(code))
}

fn respond_json(request: tiny_http::Request, code: u16, body: &str) -> std::io::Result<()> {
    let resp = Response::from_string(body)
        .with_status_code(code)
        .with_header(Header::from_bytes(&b"Content-Type"[..], &b"application/json"[..]).unwrap());
    request.respond(resp)
}

fn handle_get_run(request: tiny_http::Request, run_id: &str) -> std::io::Result<()> {
    if !is_valid_run_id(run_id) {
        return respond_text(request, 400, "Invalid run id");
    }
    let path = run_state_path(run_id);
    if path.exists() {
        match fs::read_to_string(&path) {
            Ok(content) => respond_json(request, 200, &content),
            Err(e) => respond_text(request, 500, &format!("read error: {}", e)),
        }
    } else {
        respond_json(
            request,
            200,
            &format!(r#"{{"shimRunId":"{}","status":"running"}}"#, run_id),
        )
    }
}

fn handle_post_dispatch(
    mut request: tiny_http::Request,
    agent_id: &str,
) -> std::io::Result<()> {
    if !is_valid_agent_id(agent_id) {
        return respond_text(request, 400, "Invalid agent id");
    }
    let token = config().token.as_str();
    if !token.is_empty() {
        let provided = extract_bearer(request.headers());
        if provided.as_deref() != Some(token) {
            return respond_text(request, 401, "Unauthorized");
        }
    }

    // Validate against openclaw config — reject unknown agents eagerly.
    match load_known_agent_ids() {
        Some(ids) if ids.contains(agent_id) => {}
        Some(_) => {
            return respond_text(
                request,
                404,
                &format!("Agent '{}' is not registered in openclaw config", agent_id),
            );
        }
        None => {
            return respond_text(
                request,
                503,
                "Cannot validate agent: openclaw config unreadable",
            );
        }
    }

    let mut body = String::new();
    if let Err(e) = request.as_reader().read_to_string(&mut body) {
        return respond_text(request, 400, &format!("read error: {}", e));
    }

    let run_id = match gen_run_id() {
        Ok(id) => id,
        Err(e) => return respond_text(request, 500, &format!("id gen error: {}", e)),
    };

    dispatch_agent_run(agent_id.to_string(), body, run_id.clone());

    let resp = format!(
        r#"{{"ok":true,"agentId":"{}","runId":"{}"}}"#,
        agent_id, run_id
    );
    respond_json(request, 200, &resp)
}

fn main() -> std::io::Result<()> {
    CONFIG
        .set(Config::from_env())
        .ok()
        .expect("CONFIG already initialized");
    let cfg = config();

    if cfg.token.is_empty() {
        eprintln!("agent-shim: WARNING — AGENT_SHIM_TOKEN env var not set; auth disabled");
    }

    fs::create_dir_all(&cfg.runs_dir)?;

    let server = Server::http(&cfg.bind)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e.to_string()))?;
    eprintln!(
        "agent-shim listening on http://{} (runs dir: {})",
        cfg.bind,
        cfg.runs_dir.display()
    );

    for request in server.incoming_requests() {
        let path = request.url().to_string();

        // GET /runs/<runId>
        if request.method() == &Method::Get {
            if let Some(rest) = path.strip_prefix("/runs/") {
                handle_get_run(request, rest)?;
                continue;
            }
            respond_text(request, 404, "Not Found")?;
            continue;
        }

        // POST /<agent_id>
        if request.method() == &Method::Post {
            // Reject /runs/* on POST too
            if path.starts_with("/runs/") || path == "/runs" {
                respond_text(request, 405, "Method Not Allowed")?;
                continue;
            }
            let agent_id = path.trim_start_matches('/').to_string();
            handle_post_dispatch(request, &agent_id)?;
            continue;
        }

        respond_text(request, 405, "Method Not Allowed")?;
    }

    Ok(())
}
