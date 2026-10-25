pub mod storage;
mod transcript;

use monocode_core::{mobile::{watch, MobileHost, Push}, remote::protocol::{apply_session_sync, read_session_sync, supported_providers}};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{collections::HashMap, io::Read, net::SocketAddr, path::{Path, PathBuf}, time::{Duration, Instant}};

pub const UNSUPPORTED: &str = "Unsupported on this host";
const RPC_METHODS: &[&str] = &["environment.describe", "projects.list", "sessions.list", "sessions.sync", "sessions.syncChunk", "events.read", "models.list", "commands.dispatch"];
const PHONE_METHODS: &[&str] = &["models.list", "session.configure", "session.start", "session.send", "conversation.send", "session.cancel", "conversation.read"];

#[derive(Clone)]
pub struct Rpc {
    endpoint: String,
    token: String,
    environment: String,
    agent: ureq::Agent,
}
impl Rpc {
    pub fn connect(address: SocketAddr, token_file: &Path, data_dir: &Path) -> Result<Self, String> {
        if !address.ip().is_loopback() || address.port() == 0 {
            return Err("Host RPC address must be a numeric loopback address with a port".into());
        }
        let mut rpc = Self {
            endpoint: format!("http://{address}/rpc"),
            token: storage::token(token_file)?, environment: String::new(),
            agent: ureq::AgentBuilder::new().redirects(0).try_proxy_from_env(false)
                .timeout(Duration::from_secs(5)).build(),
        };
        let descriptor = rpc.call("environment.describe", json!({"supportedProviders":supported_providers()}))?;
        if descriptor["protocolVersion"] != 1 { return Err("Incompatible host protocol".into()); }
        rpc.environment = required(&descriptor, "environmentId")?.into();
        let pin = data_dir.join("environment.json");
        if pin.exists() {
            let environment: String = storage::read(&pin)?;
            if environment != rpc.environment { return Err("Host identity changed; reconnect explicitly using a new sidecar data directory".into()); }
        } else { storage::write(&pin, &rpc.environment)?; }
        Ok(rpc)
    }

    pub fn call(&self, method: &str, params: Value) -> Result<Value, String> {
        if !RPC_METHODS.contains(&method) { return Err(UNSUPPORTED.into()); }
        if method == "commands.dispatch" && !matches!(params["type"].as_str(), Some("create" | "send" | "cancel" | "configure")) {
            return Err(UNSUPPORTED.into());
        }
        let payload = json!({"version":1,"environmentId":self.environment,"method":method,"params":params});
        let response = self.agent.post(&self.endpoint).set("Authorization", &format!("Bearer {}", self.token))
            .set("Content-Type", "application/json").send_string(&payload.to_string());
        let response = match response {
            Ok(v) => v,
            Err(ureq::Error::Status(_, v)) => v,
            Err(_) => return Err("Host RPC did not complete; retry the same command to confirm its result".into()),
        };
        let status = response.status();
        let mut bytes = Vec::new();
        response.into_reader().take(16 * 1024 * 1024 + 1).read_to_end(&mut bytes).map_err(|_| "Cannot read host response")?;
        if bytes.len() > 16 * 1024 * 1024 { return Err("Host RPC response too large".into()); }
        let value: Value = serde_json::from_slice(&bytes).map_err(|_| "Invalid host response")?;
        // Never reflect transport details or arbitrary server errors (which may contain secrets).
        if status != 200 || value.get("error").is_some() {
            return Err(format!("Host rejected {method} (HTTP {status}); check host diagnostics"));
        }
        value.get("result").cloned().ok_or_else(|| "Invalid host response".into())
    }

    fn load(&self, session: &str, known: Option<&Value>) -> Result<Value, String> {
        let fetch = |revision| read_session_sync(session, revision, |m, p| self.call(m, p));
        let sync = fetch(known.and_then(|v| v["revision"].as_u64()))?;
        apply_session_sync(known, &sync).or_else(|_| apply_session_sync(None, &fetch(None)?))
    }
}

#[derive(Serialize, Deserialize)]
struct Receipt {
    signature: String,
    create: Option<Value>,
    command: Value,
    result: Option<Value>,
}

// Catalog IDs may include a harness (pi:) before their provider-qualified ID.
fn bare_model(id: &str) -> &str {
    id.rsplit('/').next().unwrap_or(id).rsplit(':').next().unwrap_or(id)
}

#[derive(Clone)]
pub struct Bridge {
    rpc: Rpc,
    dir: PathBuf,
    harness: String,
    model: String,
    watches: HashMap<String, Poll>,
    models_allow: Option<Vec<String>>,
}
#[derive(Clone)]
struct Poll {
    session: String,
    tail: usize,
    from: Option<String>,
    incremental: bool,
    known: Option<Value>,
    baseline: Option<watch::Baseline>,
    next: Instant,
}

impl Bridge {
    pub fn new(rpc: Rpc, data_dir: &Path, harness: String, model: String) -> Result<Self, String> {
        if !supported_providers().contains(&harness.as_str()) || model.is_empty() || model.len() > 200 {
            return Err("Supply a supported default harness and a nonempty default model".into());
        }
        let dir = data_dir.join("commands");
        storage::directory(&dir)?;
        Ok(Self { rpc, dir, harness, model, watches: HashMap::new(), models_allow: None })
    }

    pub fn with_models_allow(mut self, path: &Path) -> Result<Self, String> {
        let text = std::fs::read_to_string(path).map_err(|e| format!("Cannot read models allow file: {e}"))?;
        self.models_allow = Some(text.lines().filter_map(|line| {
            let entry = line.split('#').next().unwrap_or("").trim();
            (!entry.is_empty()).then(|| bare_model(entry).to_owned())
        }).collect());
        self.check_model(&self.model)?;
        Ok(self)
    }

    fn check_model(&self, model: &str) -> Result<(), String> {
        if self.models_allow.as_ref().is_some_and(|allow| !allow.iter().any(|id| id == bare_model(model))) {
            return Err(format!("Model {model} is not in the models allow list"));
        }
        Ok(())
    }

    pub fn catalog(&self) -> Result<Value, String> {
        let projects = self.rpc.call("projects.list", json!({}))?;
        let mut mobile_projects = Vec::new();
        let mut sessions = Vec::new();
        for project in projects.as_array().ok_or("Invalid host projects")? {
            let id = required(project, "id")?;
            let page = self.rpc.call("sessions.list", json!({"projectId":id}))?;
            for summary in page.as_array().ok_or("Invalid host sessions")? {
                if !summary.is_object() { return Err("Invalid host session summary".into()); }
                let mut s = summary.clone();
                s["projectId"] = json!(id);
                s["busy"] = json!(s["status"] == "running");
                if s["branch"].is_null() { s["branch"] = json!(""); }
                if let Some(path) = s.get("worktreeCwd").cloned() { s["worktreePath"] = path; }
                sessions.push(s);
            }
            mobile_projects.push(json!({"id":id,"name":project["name"],"path":project["cwd"],"machine":"Fedora",
                "branch":"","color":"#64748b","additions":0,"deletions":0,
                "defaultHarness":self.harness,"defaultModel":self.model,"defaultRuntimeMode":"supervised"}));
        }
        sessions.sort_by_key(|s| std::cmp::Reverse(s["updatedAt"].as_u64().unwrap_or(0)));
        Ok(json!({"projects":mobile_projects,"monos":[],"sessions":sessions}))
    }

    fn models(&self, params: &Value) -> Result<Value, String> {
        let project = required(params, "projectId")?;
        let mut current = json!({"harness":self.harness,"model":self.model,"modelSettings":{},"runtimeMode":"supervised"});
        if params.get("conversationId").is_some() {
            let snapshot = self.rpc.load(session_id(params)?, None)?;
            if snapshot["projectId"] != project { return Err("Session does not belong to this project".into()); }
            for key in ["harness", "model", "modelSettings", "runtimeMode"] {
                if let Some(value) = snapshot["session"].get(key) { current[key] = value.clone(); }
            }
        }
        let catalog = self.rpc.call("models.list", json!({"projectId":project}))?;
        let by_harness = catalog["models"].as_object().ok_or("Invalid host model catalog")?;
        let mut harnesses = Vec::new();
        let mut models = Vec::new();
        for &harness in supported_providers() {
            let Some(entries) = by_harness.get(harness) else { continue; };
            let entries = entries.as_array().ok_or("Invalid host model catalog")?;
            if entries.is_empty() { continue; }
            let label = match harness {
                "claude" => "Claude Code", "codex" => "Codex", "cursor" => "Cursor",
                "grok" => "Grok Build", "opencode" => "OpenCode", "pi" => "Pi",
                "hermes" => "Hermes Agent", "antigravity" => "Antigravity", "devin" => "Devin",
                other => other,
            };
            harnesses.push(json!({"id":harness,"label":label}));
            for model in entries {
                let mut entry = json!({"id":required(model,"id")?,"harness":harness,"name":required(model,"name")?,
                    "settings":model.get("settings").cloned().unwrap_or_else(|| json!([]))});
                if let Some(provider) = model["provider"]["name"].as_str() { entry["provider"] = json!(provider); }
                models.push(entry);
            }
        }
        if models.is_empty() { return Err("Host has no available models; check host provider diagnostics".into()); }
        if self.models_allow.is_some() {
            let filtered: Vec<_> = models.iter().filter(|m| self.check_model(m["id"].as_str().unwrap_or("")).is_ok()).cloned().collect();
            if filtered.is_empty() {
                eprintln!("Models allow list matched no catalog models; returning unfiltered models to avoid an empty picker");
            } else {
                models = filtered;
                harnesses.retain(|h| models.iter().any(|m| m["harness"] == h["id"]));
            }
        }
        // The phone expects effective values, including each model's defaults.
        let mut settings = serde_json::Map::new();
        if let Some(model) = models.iter().find(|m| m["id"] == current["model"] && m["harness"] == current["harness"]) {
            for setting in model["settings"].as_array().ok_or("Invalid host model settings")? {
                settings.insert(required(setting, "id")?.into(), setting["value"].clone());
            }
        }
        if let Some(saved) = current["modelSettings"].as_object() { settings.extend(saved.clone()); }
        current["modelSettings"] = Value::Object(settings);
        bounded(json!({"harnesses":harnesses,"models":models,"current":current,"runtimeModes":[
            {"id":"supervised","label":"Supervised","hint":"Ask before commands and file changes."},
            {"id":"auto-accept-edits","label":"Auto-accept edits","hint":"Auto-approve edits, ask before other actions."},
            {"id":"auto","label":"Auto","hint":"An AI reviewer can approve or deny actions."},
            {"id":"full-access","label":"Full access","hint":"Allow commands, edits, and supported MCP confirmations in non-plan turns without prompts."}
        ]}))
    }

    pub fn request(&self, id: &str, method: &str, params: &Value) -> Result<Value, String> {
        if !PHONE_METHODS.contains(&method) { return Err(UNSUPPORTED.into()); }
        if !params.is_object() { return Err("Invalid mobile parameters".into()); }
        if method == "models.list" { return self.models(params); }
        // Check even replayed commands when policy has changed since their receipt.
        if method == "session.start" {
            self.check_model(params["model"].as_str().unwrap_or(&self.model))?;
        } else if method == "session.configure" {
            if let Some(model) = params.get("model") { self.check_model(model.as_str().ok_or("Invalid model")?)?; }
        }
        if method == "conversation.read" {
            let session = session_id(params)?;
            let snapshot = self.rpc.load(session, None)?;
            let turns = transcript::turns(&snapshot)?;
            let start = turns.len().saturating_sub(100);
            return bounded(json!({"id":session,"messages":[],"turns":&turns[start..],"busy":snapshot["status"] == "running"}));
        }
        if id.is_empty() || id.len() > 256 { return Err("Invalid command ID".into()); }
        let signature = digest(&json!([method, params]).to_string());
        let key = digest(id);
        let path = self.dir.join(format!("{key}.json"));
        let mut receipt: Receipt = if path.exists() {
            let receipt: Receipt = storage::read(&path)?;
            if receipt.signature != signature { return Err("Command ID already used with different input".into()); }
            receipt
        } else {
            let (create, command) = self.commands(&key, method, params)?;
            let receipt = Receipt { signature, create, command, result: None };
            storage::write(&path, &receipt)?;
            receipt
        };
        if method == "session.configure" {
            self.check_model(required(&receipt.command, "model")?)?;
        }
        if let Some(result) = receipt.result { return Ok(result); }
        if let Some(create) = &receipt.create {
            if !receipt.command.is_object() { return Err("Invalid saved sidecar command".into()); }
            // Host receipts make BOTH phases idempotent, even after a crash/lost reply.
            let created = self.rpc.call("commands.dispatch", create.clone())?;
            let session = required(&created, "sessionId")?;
            receipt.command["sessionId"] = json!(session);
            storage::write(&path, &receipt)?;
        }
        let result = if receipt.command.is_null() {
            json!({"sessionId":session_id(params)?})
        } else {
            let result = self.rpc.call("commands.dispatch", receipt.command.clone())?;
            json!({"sessionId":required(&result, "sessionId")?})
        };
        receipt.result = Some(result.clone());
        storage::write(&path, &receipt)?;
        Ok(result)
    }

    fn commands(&self, key: &str, method: &str, params: &Value) -> Result<(Option<Value>, Value), String> {
        if let Some(attachments) = params.get("attachments") {
            if !attachments.as_array().is_some_and(|a| a.is_empty()) {
                return Err("Attachments are unsupported on this host".into());
            }
        }
        let command_id = |phase: &str| format!("phone:{key}:{phase}");
        if method == "session.configure" {
            if params.get("providerAccountId").is_some() {
                return Err("Account switching is unsupported on this host".into());
            }
            let session = session_id(params)?;
            let state = self.rpc.load(session, None)?;
            if state["status"] == "running" {
                return Err("Wait for the current turn before changing settings".into());
            }
            let current = &state["session"];
            let harness = required(current, "harness")?;
            if params.get("harness").is_some_and(|v| v.as_str() != Some(harness)) {
                return Err("Changing a session's harness is unsupported on this host; start a new session with that harness".into());
            }
            // Host configure replaces all three fields. Freeze the merged choice
            // in the receipt so retries never rebase onto a later configuration.
            let mut command = json!({"type":"configure","commandId":command_id("configure"),"sessionId":session,
                "model":required(current,"model")?,"modelSettings":current.get("modelSettings").cloned().unwrap_or_else(|| json!({})),
                "runtimeMode":current.get("runtimeMode").cloned().unwrap_or_else(|| json!("supervised"))});
            for key in ["model", "modelSettings", "runtimeMode"] {
                if let Some(value) = params.get(key) { command[key] = value.clone(); }
            }
            let model = required(&command, "model")?;
            self.check_model(model)?;
            if model.split_once(':').is_some_and(|(provider, _)| provider != harness) {
                return Err("Model must belong to the session's harness; start a new session to change harness".into());
            }
            return Ok((None, command));
        }
        if method == "session.cancel" {
            let session = session_id(params)?;
            let state = self.rpc.load(session, None)?;
            let command = if state["status"] == "running" {
                json!({"type":"cancel","commandId":command_id("cancel"),"sessionId":session,"runId":required(&state,"runId")?})
            } else { Value::Null };
            return Ok((None, command));
        }
        let text = required(params, "text")?;
        if text.encode_utf16().count() > 256_000 || text.contains('\0') { return Err("Invalid prompt".into()); }
        let session = if method == "session.start" { "" } else { session_id(params)? };
        let command = json!({"type":"send","commandId":command_id("send"),"sessionId":session,"text":text});
        let create = if method == "session.start" {
            let harness = params["harness"].as_str().unwrap_or(&self.harness);
            let model = params["model"].as_str().unwrap_or(&self.model);
            let mode = params["runtimeMode"].as_str().unwrap_or("supervised");
            if !supported_providers().contains(&harness) || model.is_empty() || model.len() > 200 || !["supervised","auto-accept-edits","auto","full-access"].contains(&mode) {
                return Err("Invalid session configuration".into());
            }
            let mut create = json!({"type":"create","commandId":command_id("create"),"projectId":required(params,"projectId")?,
                "harness":harness,"model":model,"runtimeMode":mode});
            if let Some(settings) = params.get("modelSettings") { create["modelSettings"] = settings.clone(); }
            if let Some(cwd) = params.get("worktreeCwd") { create["worktreeCwd"] = cwd.clone(); }
            Some(create)
        } else { None };
        Ok((create, command))
    }

    pub fn watch(&mut self, id: String, session: String, tail: Option<u64>, from: Option<String>, incremental: bool) -> Result<(), String> {
        if self.watches.len() >= 64 { return Err("Host watch limit reached".into()); }
        if session.starts_with("mono:") { return Err(UNSUPPORTED.into()); }
        let known = self.rpc.load(&session, None)?;
        self.watches.insert(id, Poll { session, tail: tail.unwrap_or(20).clamp(1, 100) as usize, from, incremental,
            known: Some(known), baseline: None, next: Instant::now() });
        Ok(())
    }
    pub fn stop_watch(&mut self, id: &str) { self.watches.remove(id); }

    pub fn poll(&mut self, host: &MobileHost) {
        self.watches.retain(|id, poll| {
            if poll.next > Instant::now() { return true; }
            poll.next = Instant::now() + Duration::from_millis(750);
            let result: Result<bool, String> = (|| {
                // Sync remains authoritative, including when bounded event history
                // no longer contains our old cursor. Always check RPC, even when
                // retrying a first frame or recovering from a transient error.
                self.rpc.call("events.read", json!({"sessionId":poll.session,"after":poll.known.as_ref().and_then(|s| s["revision"].as_u64()).unwrap_or(0)}))?;
                poll.known = Some(self.rpc.load(&poll.session, poll.known.as_ref())?);
                let snapshot = poll.known.as_ref().ok_or("Missing session snapshot")?;
                let turns = transcript::turns(snapshot)?;
                let ids = turns.iter().filter_map(|t| t["id"].as_str().map(str::to_owned)).collect::<Vec<_>>();
                let (start, from, older) = watch::window(&ids, poll.from.as_deref(), poll.tail);
                poll.from = from;
                if let Some((frame, baseline)) = watch::update(poll.baseline.as_ref(), snapshot["status"] == "running", older, &turns[start..], poll.incremental) {
                    match host.push(id, bounded(frame)?) {
                        Ok(()) => poll.baseline = Some(baseline),
                        Err(Push::Busy) => (),
                        Err(Push::Gone) => return Ok(false),
                    }
                }
                Ok(true)
            })();
            match result {
                Ok(keep) => keep,
                Err(error) => {
                    // A recovery frame must clear the phone's error even when the
                    // host revision did not change while it was unreachable.
                    poll.baseline = None;
                    !matches!(host.push(id, json!({"error":error})), Err(Push::Gone))
                }
            }
        });
    }
}
fn required<'a>(value: &'a Value, key: &str) -> Result<&'a str, String> {
    value[key].as_str().filter(|s| !s.trim().is_empty() && !s.contains('\0')).ok_or_else(|| format!("Missing or invalid {key}"))
}
fn session_id(params: &Value) -> Result<&str, String> {
    let id = params.get("conversationId").or_else(|| params.get("sessionId")).and_then(Value::as_str)
        .filter(|s| !s.is_empty() && s.len() <= 256).ok_or("Missing conversationId")?;
    if id.starts_with("mono:") { return Err(UNSUPPORTED.into()); }
    Ok(id)
}
fn bounded(value: Value) -> Result<Value, String> {
    if value.to_string().len() > monocode_mobile_link::RESPONSE_LIMIT - 1024 {
        return Err("Conversation exceeds phone response limit; request a smaller watch tail".into());
    }
    Ok(value)
}
fn digest(value: &str) -> String { format!("{:x}", Sha256::digest(value.as_bytes())) }
