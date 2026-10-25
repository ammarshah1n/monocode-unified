//! All listeners/clients bind loopback; all files live under this crate's target.
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use monocode_core::mobile::{Event, MobileHost};
use monocode_mobile_link::{Identity, RESPONSE_LIMIT};
use monocode_phone_sidecar::{storage, Bridge, Rpc, UNSUPPORTED};
use serde_json::{json, Value};
use std::{collections::HashMap, fs, io::{Read, Write}, net::{SocketAddr, TcpListener, UdpSocket}, os::unix::fs::{OpenOptionsExt, PermissionsExt}, path::Path, sync::{atomic::{AtomicBool, Ordering}, Arc, Mutex}, time::Duration};

fn fixture() -> tempfile::TempDir {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/test-fixtures");
    fs::create_dir_all(&root).unwrap();
    tempfile::tempdir_in(root).unwrap()
}
fn token(dir: &Path) -> std::path::PathBuf {
    let path = dir.join("host-token");
    let mut f = fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&path).unwrap();
    // Disposable protocol credential, deliberately not a real host token.
    write!(f, "{:_<43}", "fixture").unwrap();
    path
}

fn host_models() -> Value {
    json!({"models":{
        "codex":[{"id":"codex:fixture","harness":"codex","name":"Fixture Codex","settings":[
            {"id":"reasoningEffort","label":"Effort","kind":"select","value":"medium","options":[
                {"value":"medium","label":"Medium"},{"value":"high","label":"High"}]}]},
            {"id":"codex:other","harness":"codex","name":"Other Codex"}],
        "pi":[{"id":"pi:fixture","harness":"pi","name":"Fixture Pi","provider":{"id":"fixture","name":"Fixture Provider"}}]
    },"errors":{"claude":"private provider diagnostic"}})
}

#[derive(Default)]
struct State {
    commands: HashMap<String, (Value, Value)>,
    create_count: usize,
    send_count: usize,
    cancel_count: usize,
    configure_count: usize,
    methods: Vec<String>,
    lose_reply: Option<String>,
    fail_method: Option<String>,
    snapshot: Value,
}
struct Fake {
    address: SocketAddr,
    state: Arc<Mutex<State>>,
    done: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}
impl Fake {
    fn new(lose: Option<&str>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let address = listener.local_addr().unwrap();
        let state = Arc::new(Mutex::new(State { lose_reply: lose.map(str::to_owned), snapshot: json!({
            "projectId":"p", "revision":0,"status":"idle","updatedAt":123,
            "session":{"id":"s","harness":"codex","model":"codex:fixture","modelSettings":{"reasoningEffort":"high"},"runtimeMode":"supervised","blocks":[]}
        }), ..State::default() }));
        let done = Arc::new(AtomicBool::new(false));
        let (s, d) = (state.clone(), done.clone());
        let thread = std::thread::spawn(move || {
            while !d.load(Ordering::SeqCst) {
                match listener.accept() {
                    Ok((mut stream, peer)) => {
                        assert!(peer.ip().is_loopback());
                        stream.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
                        let mut bytes = Vec::new();
                        let header_end = loop {
                            let mut b = [0; 1024];
                            let n = stream.read(&mut b).unwrap();
                            if n == 0 { break 0; }
                            bytes.extend_from_slice(&b[..n]);
                            if let Some(i) = bytes.windows(4).position(|w| w == b"\r\n\r\n") { break i + 4; }
                        };
                        if header_end == 0 { continue; }
                        let headers = String::from_utf8_lossy(&bytes[..header_end]).to_lowercase();
                        assert!(headers.starts_with("post /rpc http/1.1"));
                        assert!(headers.contains(&format!("authorization: bearer {:_<43}", "fixture")));
                        assert!(!headers.contains("\r\norigin:"));
                        let length: usize = headers.lines().find_map(|l| l.strip_prefix("content-length:")).unwrap().trim().parse().unwrap();
                        while bytes.len() - header_end < length {
                            let mut b = [0; 4096];
                            let n = stream.read(&mut b).unwrap();
                            assert!(n > 0);
                            bytes.extend_from_slice(&b[..n]);
                        }
                        let input: Value = serde_json::from_slice(&bytes[header_end..header_end + length]).unwrap();
                        assert_eq!(input["version"], 1);
                        let method = input["method"].as_str().unwrap();
                        let p = &input["params"];
                        let mut state = s.lock().unwrap();
                        state.methods.push(method.into());
                        if method != "environment.describe" { assert_eq!(input["environmentId"], "fake-environment"); }
                        if state.fail_method.as_deref() == Some(method) {
                            state.fail_method = None;
                            let body = "{\"error\":\"fixture unavailable\"}";
                            let _ = write!(stream, "HTTP/1.1 503 Service Unavailable\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
                            continue;
                        }
                        let result = match method {
                            "environment.describe" => json!({"protocolVersion":1,"environmentId":"fake-environment"}),
                            "projects.list" => json!([{"id":"p","cwd":"/fixture","name":"Fixture"}]),
                            "models.list" => {
                                assert_eq!(p, &json!({"projectId":"p"}));
                                host_models()
                            },
                            "sessions.list" => if state.create_count == 0 { json!([]) } else {
                                json!([{"id":"s","title":"Fixture session","harness":"codex","model":"codex:fixture","updatedAt":123,"status":state.snapshot["status"]}])
                            },
                            "sessions.sync" => if p["revision"] == state.snapshot["revision"] { json!({"kind":"unchanged","revision":p["revision"]}) } else { json!({"kind":"snapshot","value":state.snapshot}) },
                            "events.read" => json!([]),
                            "commands.dispatch" => {
                                let id = p["commandId"].as_str().unwrap().to_string();
                                assert!(id.len() <= 128);
                                let kind = p["type"].as_str().unwrap();
                                if let Some((old, result)) = state.commands.get(&id) {
                                    assert_eq!(old, p, "Retries must preserve the exact command");
                                    result.clone()
                                } else {
                                    match kind {
                                        "create" => {
                                            state.create_count += 1;
                                            for key in ["harness", "model", "modelSettings", "runtimeMode"] {
                                                if let Some(value) = p.get(key) { state.snapshot["session"][key] = value.clone(); }
                                            }
                                        }
                                        "configure" => {
                                            assert_eq!(p["sessionId"], "s");
                                            assert_ne!(state.snapshot["status"], "running");
                                            assert!(p.get("harness").is_none());
                                            state.configure_count += 1;
                                            for key in ["model", "modelSettings", "runtimeMode"] {
                                                state.snapshot["session"][key] = p[key].clone();
                                            }
                                            let revision = state.snapshot["revision"].as_u64().unwrap() + 1;
                                            state.snapshot["revision"] = json!(revision);
                                        }
                                        "send" => {
                                            state.send_count += 1;
                                            assert_eq!(p["sessionId"], "s");
                                            state.snapshot["revision"] = json!(1);
                                            state.snapshot["status"] = json!("running");
                                            state.snapshot["runId"] = json!("run-1");
                                            state.snapshot["session"]["blocks"] = json!([
                                                {"id":"user","role":"user","text":p["text"],"startedAt":123},
                                                {"id":"assistant","role":"assistant","text":"working","streaming":true}
                                            ]);
                                        }
                                        "cancel" => { state.cancel_count += 1; state.snapshot["status"] = json!("idle"); }
                                        _ => panic!("Non-allowlisted command"),
                                    }
                                    let result = json!({"commandId":id,"sessionId":"s","revision":state.snapshot["revision"]});
                                    state.commands.insert(id, (p.clone(), result.clone()));
                                    result
                                }
                            }
                            _ => panic!("Non-allowlisted RPC method {method}"),
                        };
                        if method == "commands.dispatch" && state.lose_reply.as_deref() == p["type"].as_str() {
                            state.lose_reply = None;
                            // Effect/receipt committed, but the client never sees the reply.
                            continue;
                        }
                        drop(state);
                        let body = json!({"result":result}).to_string();
                        let _ = write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => std::thread::sleep(Duration::from_millis(2)),
                    Err(e) => panic!("{e}"),
                }
            }
        });
        Self { address, state, done, thread: Some(thread) }
    }
    fn bridge(&self, dir: &Path, token: &Path) -> Bridge {
        let rpc = Rpc::connect(self.address, token, dir).unwrap();
        Bridge::new(rpc, dir, "codex".into(), "codex:fixture".into()).unwrap()
    }
}
impl Drop for Fake {
    fn drop(&mut self) {
        self.done.store(true, Ordering::SeqCst);
        self.thread.take().unwrap().join().unwrap();
    }
}

#[test]
fn create_then_send_is_idempotent_across_lost_replies_and_sidecar_restarts() {
    for phase in ["create", "send"] {
        let dir = fixture();
        let token = token(dir.path());
        let server = Fake::new(Some(phase));
        let params = json!({"projectId":"p","text":"hello"});
        let bridge = server.bridge(dir.path(), &token);
        assert!(bridge.request("phone-device:retry", "session.start", &params).is_err());
        drop(bridge);
        let bridge = server.bridge(dir.path(), &token);
        assert_eq!(bridge.request("phone-device:retry", "session.start", &params).unwrap()["sessionId"], "s");
        assert!(bridge.request("phone-device:retry", "session.start", &json!({"projectId":"p","text":"changed"})).is_err());
        bridge.request("phone-device:retry", "session.start", &params).unwrap();
        let state = server.state.lock().unwrap();
        assert_eq!((state.create_count, state.send_count), (1, 1));
        drop(state);
        let catalog = bridge.catalog().unwrap();
        assert_eq!(catalog["projects"][0]["path"], "/fixture");
        assert_eq!(catalog["sessions"][0]["busy"], true);
        let read = bridge.request("read", "conversation.read", &json!({"conversationId":"s"})).unwrap();
        assert_eq!(read["turns"][0]["items"][0]["text"], "hello");
        bridge.request("cancel", "session.cancel", &json!({"conversationId":"s"})).unwrap();
        // A duplicate cancel must never stop a later run.
        server.state.lock().unwrap().snapshot["runId"] = json!("run-2");
        server.state.lock().unwrap().snapshot["status"] = json!("running");
        bridge.request("cancel", "session.cancel", &json!({"conversationId":"s"})).unwrap();
        assert_eq!(server.state.lock().unwrap().cancel_count, 1);
    }
}

#[test]
fn models_adapt_host_catalog_and_use_current_session_or_new_session_defaults() {
    let dir = fixture();
    let token = token(dir.path());
    let server = Fake::new(None);
    let bridge = server.bridge(dir.path(), &token);
    let catalog = bridge.request("read", "models.list", &json!({"projectId":"p"})).unwrap();
    assert_eq!(catalog["harnesses"], json!([{"id":"codex","label":"Codex"},{"id":"pi","label":"Pi"}]));
    assert_eq!(catalog["models"][0]["settings"], host_models()["models"]["codex"][0]["settings"]);
    assert_eq!(catalog["models"][1]["settings"], json!([]));
    assert_eq!(catalog["models"][2]["provider"], "Fixture Provider");
    assert_eq!(catalog["runtimeModes"].as_array().unwrap().len(), 4);
    assert_eq!(catalog["current"], json!({"harness":"codex","model":"codex:fixture","modelSettings":{"reasoningEffort":"medium"},"runtimeMode":"supervised"}));
    assert!(!catalog.to_string().contains("private provider diagnostic"));
    let current = bridge.request("read", "models.list", &json!({"projectId":"p","conversationId":"s"})).unwrap();
    assert_eq!(current["current"]["modelSettings"]["reasoningEffort"], "high");
    assert!(bridge.request("read", "models.list", &json!({"projectId":"other","conversationId":"s"})).is_err());
    assert!(bridge.request("read", "models.list", &json!({"projectId":"p","conversationId":"mono:x"})).is_err());
    assert!(fs::read_dir(dir.path().join("commands")).unwrap().next().is_none());
    server.state.lock().unwrap().fail_method = Some("models.list".into());
    let error = bridge.request("read", "models.list", &json!({"projectId":"p"})).unwrap_err();
    assert!(error.contains("Host rejected models.list"));
    assert!(!error.contains("fixture unavailable"));
}

#[test]
fn models_allow_filters_and_rejects_disallowed_commands() {
    let dir = fixture();
    let token = token(dir.path());
    let server = Fake::new(None);
    let path = dir.path().join("allow.txt");
    for entry in ["fixture", "provider/fixture", "pi:provider/fixture"] {
        fs::write(&path, format!("# comment\n\n{entry} # inline\n")).unwrap();
        let bridge = server.bridge(dir.path(), &token).with_models_allow(&path).unwrap();
        let catalog = bridge.request("read", "models.list", &json!({"projectId":"p"})).unwrap();
        assert_eq!(catalog["models"].as_array().unwrap().len(), 2);
        for (method, params) in [
            ("session.start", json!({"projectId":"p","text":"hello","model":"codex:other"})),
            ("session.configure", json!({"conversationId":"s","model":"codex:other"})),
        ] {
            assert!(bridge.request("blocked", method, &params).unwrap_err().contains("models allow list"));
        }
        bridge.request("allowed", "session.configure", &json!({"conversationId":"s","model":"codex:fixture"})).unwrap();
    }
    fs::write(&path, "missing\n").unwrap();
    assert!(server.bridge(dir.path(), &token).with_models_allow(&path).is_err());
    let rpc = Rpc::connect(server.address, &token, dir.path()).unwrap();
    let bridge = Bridge::new(rpc, dir.path(), "codex".into(), "missing".into()).unwrap().with_models_allow(&path).unwrap();
    let catalog = bridge.request("read", "models.list", &json!({"projectId":"p"})).unwrap();
    assert_eq!(catalog["models"].as_array().unwrap().len(), 3);
    fs::write(&path, "# empty\n").unwrap();
    assert!(server.bridge(dir.path(), &token).with_models_allow(&path).is_err());
    assert!(server.bridge(dir.path(), &token).with_models_allow(&dir.path().join("absent")).is_err());
}

#[test]
fn configure_merges_partial_choice_and_retries_the_frozen_command_after_restart() {
    let dir = fixture();
    let token = token(dir.path());
    let server = Fake::new(Some("configure"));
    let bridge = server.bridge(dir.path(), &token);
    let params = json!({"conversationId":"s","model":"codex:other"});
    assert!(bridge.request("configure", "session.configure", &params).is_err());
    {
        let mut state = server.state.lock().unwrap();
        assert_eq!(state.configure_count, 1);
        assert_eq!(state.snapshot["session"]["model"], "codex:other");
        assert_eq!(state.snapshot["session"]["modelSettings"], json!({"reasoningEffort":"high"}));
        assert_eq!(state.snapshot["session"]["runtimeMode"], "supervised");
        // Another client changes the session before the lost-reply retry.
        state.snapshot["session"]["runtimeMode"] = json!("auto");
        state.snapshot["session"]["modelSettings"] = json!({"reasoningEffort":"low"});
    }
    drop(bridge);
    let bridge = server.bridge(dir.path(), &token);
    assert_eq!(bridge.request("configure", "session.configure", &params).unwrap(), json!({"sessionId":"s"}));
    bridge.request("configure", "session.configure", &params).unwrap();
    assert_eq!(server.state.lock().unwrap().configure_count, 1);
    assert!(bridge.request("configure", "session.configure", &json!({"conversationId":"s","model":"codex:fixture"})).is_err());
    bridge.request("effort", "session.configure", &json!({"conversationId":"s","modelSettings":{"reasoningEffort":"high"}})).unwrap();
    bridge.request("access", "session.configure", &json!({"conversationId":"s","runtimeMode":"auto-accept-edits"})).unwrap();
    let state = server.state.lock().unwrap();
    assert_eq!(state.snapshot["session"]["model"], "codex:other");
    assert_eq!(state.snapshot["session"]["modelSettings"], json!({"reasoningEffort":"high"}));
    assert_eq!(state.snapshot["session"]["runtimeMode"], "auto-accept-edits");
    drop(state);
    for choice in [json!({"harness":"pi"}), json!({"model":"pi:fixture"}), json!({"providerAccountId":"other"})] {
        let mut params = choice;
        params["conversationId"] = json!("s");
        assert!(bridge.request("unsupported-choice", "session.configure", &params).is_err());
    }
    server.state.lock().unwrap().snapshot["status"] = json!("running");
    let error = bridge.request("busy", "session.configure", &json!({"conversationId":"s","model":"codex:fixture"})).unwrap_err();
    assert!(error.contains("Wait for the current turn"));
    assert_eq!(server.state.lock().unwrap().configure_count, 3);
}

#[test]
fn new_session_passes_through_explicit_model_harness_settings_and_runtime_mode() {
    let dir = fixture();
    let token = token(dir.path());
    let server = Fake::new(None);
    let bridge = server.bridge(dir.path(), &token);
    let params = json!({"projectId":"p","text":"hello","harness":"pi","model":"pi:fixture",
        "modelSettings":{"thinkingLevel":"high"},"runtimeMode":"auto-accept-edits"});
    bridge.request("chosen", "session.start", &params).unwrap();
    let state = server.state.lock().unwrap();
    let create = &state.commands.values().find(|(command, _)| command["type"] == "create").unwrap().0;
    for key in ["harness", "model", "modelSettings", "runtimeMode"] { assert_eq!(create[key], params[key]); }
}

#[test]
fn unsupported_methods_never_reach_rpc_and_credentials_are_private() {
    let dir = fixture();
    let token = token(dir.path());
    let server = Fake::new(None);
    let bridge = server.bridge(dir.path(), &token);
    let before = server.state.lock().unwrap().methods.len();
    for method in ["accounts.usage", "project.fileHighlight", "monos.list", "approvals.approve", "workspace.run", "commands.dispatch", "shell.exec"] {
        assert_eq!(bridge.request("id", method, &json!({})).unwrap_err(), UNSUPPORTED);
    }
    assert_eq!(before, server.state.lock().unwrap().methods.len());
    let rpc = Rpc::connect(server.address, &token, dir.path()).unwrap();
    let before = server.state.lock().unwrap().methods.len();
    for method in ["sessions.update", "sessions.delete", "files.read", "workspace.run", "shell.exec"] {
        assert_eq!(rpc.call(method, json!({})).unwrap_err(), UNSUPPORTED);
    }
    for kind in ["approve", "answer", "compact", "draft", "removeDraft", "shell.exec"] {
        assert_eq!(rpc.call("commands.dispatch", json!({"type":kind})).unwrap_err(), UNSUPPORTED);
    }
    assert_eq!(before, server.state.lock().unwrap().methods.len());
    fs::set_permissions(&token, fs::Permissions::from_mode(0o644)).unwrap();
    assert!(storage::token(&token).is_err());
    fs::set_permissions(&token, fs::Permissions::from_mode(0o600)).unwrap();
    let link = dir.path().join("token-link");
    std::os::unix::fs::symlink(&token, &link).unwrap();
    assert!(storage::token(&link).is_err());
    assert!(Rpc::connect("192.0.2.1:3774".parse().unwrap(), &token, dir.path()).is_err());
    let identity = storage::identity(dir.path()).unwrap();
    assert_eq!(storage::identity(dir.path()).unwrap().fingerprint().unwrap(), identity.fingerprint().unwrap());
    assert_eq!(fs::metadata(dir.path().join("identity.json")).unwrap().permissions().mode() & 0o777, 0o600);
    let lock = storage::lock(dir.path()).unwrap();
    assert!(storage::lock(dir.path()).is_err());
    drop(lock);
    assert!(storage::lock(dir.path()).is_ok());
}

// Client transport intentionally does NOT use mobile-link::exchange, whose
// generic desktop client binds [::]:0. Even client sockets stay loopback here.
async fn client(identity: &Identity, server: &Identity, address: SocketAddr) -> (quinn::Endpoint, quinn::Connection) {
    use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
    let mut roots = rustls::RootCertStore::empty();
    roots.add(CertificateDer::from(server.certificate_der().unwrap())).unwrap();
    let mut tls = rustls::ClientConfig::builder().with_root_certificates(roots)
        .with_client_auth_cert(vec![CertificateDer::from(identity.certificate_der().unwrap())],
            PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(identity.key_der().unwrap()))).unwrap();
    tls.alpn_protocols = vec![monocode_mobile_link::ALPN.to_vec()];
    let config = quinn::ClientConfig::new(Arc::new(quinn::crypto::rustls::QuicClientConfig::try_from(tls).unwrap()));
    let mut endpoint = quinn::Endpoint::client("127.0.0.1:0".parse().unwrap()).unwrap();
    endpoint.set_default_client_config(config);
    let connection = tokio::time::timeout(Duration::from_secs(3), endpoint.connect(address, "monocode.local").unwrap()).await.unwrap().unwrap();
    (endpoint, connection)
}
async fn exchange(connection: &quinn::Connection, method: &str, id: &str, params: Value) -> Result<Value, String> {
    let (mut send, mut recv) = connection.open_bi().await.map_err(|e| e.to_string())?;
    monocode_mobile_link::write_frame(&mut send, &json!({"version":1,"id":id,"method":method,"params":params}), monocode_mobile_link::REQUEST_LIMIT).await?;
    let response = tokio::time::timeout(Duration::from_secs(5), monocode_mobile_link::read_frame(&mut recv, RESPONSE_LIMIT)).await.map_err(|_| "Timeout")??;
    if response["ok"] == true { Ok(response["result"].clone()) } else { Err(response["error"].as_str().unwrap_or("Error").into()) }
}
fn invitation(host: &MobileHost) -> Identity {
    let uri = host.invite().unwrap().url;
    let bytes = URL_SAFE_NO_PAD.decode(uri.split_once("?i=").unwrap().1).unwrap();
    let value: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(value["addresses"].as_array().unwrap().len(), 1);
    assert_eq!(value["addresses"][0]["host"], "127.0.0.1");
    serde_json::from_value(value["identity"].clone()).unwrap()
}

#[test]
fn loopback_pairing_requests_watches_leave_and_revocation() {
    let dir = fixture();
    let token = token(dir.path());
    let fake = Fake::new(None);
    let bridge = fake.bridge(dir.path(), &token);
    let identity = storage::identity(dir.path()).unwrap();
    for bind in ["0.0.0.0:0", "[::]:0", "[::ffff:0.0.0.0]:0", "255.255.255.255:0", "224.0.0.1:0"] {
        assert!(MobileHost::open_at(bind.parse().unwrap(), dir.path().join("bad.json"), identity.clone()).is_err());
    }
    let settings = dir.path().join("mobile-access.json");
    let (host, events) = MobileHost::open_at("127.0.0.1:0".parse().unwrap(), settings.clone(), identity.clone()).unwrap();
    host.configure(true, true, "other.example:38416").unwrap();
    assert!(!host.status().open_router);
    assert_eq!(host.status().router_status, "Off");
    let address = SocketAddr::new("127.0.0.1".parse().unwrap(), host.status().addresses[0].port);
    let _other = UdpSocket::bind(SocketAddr::new("127.0.0.2".parse().unwrap(), address.port())).expect("Must not bind a wildcard socket");
    host.publish(bridge.catalog().unwrap()).unwrap();
    let running = Arc::new(AtomicBool::new(true));
    let (r, h) = (running.clone(), host.clone());
    let worker = std::thread::spawn(move || {
        let mut bridge = bridge;
        while r.load(Ordering::SeqCst) {
            while let Ok(event) = events.try_recv() {
                match event {
                    Event::Request(req) => {
                        let result = bridge.request(&req.command_id, &req.method, &req.params);
                        req.reply(result);
                    }
                    Event::Watch(start) => {
                        let result = bridge.watch(start.id.clone(), start.conversation_id.clone(), start.tail, start.from.clone(), start.incremental);
                        start.accept(result);
                    }
                    Event::WatchStop(id) => bridge.stop_watch(&id),
                }
            }
            bridge.poll(&h);
            std::thread::sleep(Duration::from_millis(10));
        }
    });
    let phone = Identity::generate().unwrap();
    let join = invitation(&host);
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    let device = runtime.block_on(async {
        let (_ep, c) = client(&join, &identity, address).await;
        let proof = phone.proof(&join.fingerprint().unwrap()).unwrap();
        let enrolled = exchange(&c, "device.enroll", "enroll", json!({"certificate":phone.certificate,"proof":proof,"name":"Fixture phone"})).await.unwrap();
        assert!(exchange(&c, "device.enroll", "again", json!({"certificate":phone.certificate,"proof":proof})).await.is_err());
        let (_ep2, c) = client(&phone, &identity, address).await;
        let catalog = exchange(&c, "workspace.list", "list", json!({})).await.unwrap();
        assert_eq!(catalog["projects"][0]["id"], "p");
        assert_eq!(catalog["projects"][0]["machine"], "Fedora");
        let models = exchange(&c, "models.list", "models", json!({"projectId":"p"})).await.unwrap();
        assert_eq!(models["models"][0]["id"], "codex:fixture");
        let configured = exchange(&c, "session.configure", "configure", json!({"conversationId":"s","harness":"codex","model":"codex:other","modelSettings":{"reasoningEffort":"high"}})).await.unwrap();
        assert_eq!(configured["sessionId"], "s");
        for method in ["accounts.usage", "project.fileHighlight", "monos.list", "approvals.approve", "shell.exec"] {
            assert_eq!(exchange(&c, method, "unsupported", json!({})).await.unwrap_err(), UNSUPPORTED);
        }
        let started = exchange(&c, "session.start", "start", json!({"projectId":"p","text":"from phone"})).await.unwrap();
        assert_eq!(started["sessionId"], "s");
        let (mut send, mut recv) = c.open_bi().await.unwrap();
        // Keep the send half open for the lifetime of the watch.
        let watch = json!({"version":1,"watch":true,"method":"conversation.watch","params":{"conversationId":"s","tail":20,"incremental":true}});
        let bytes = serde_json::to_vec(&watch).unwrap();
        use tokio::io::AsyncWriteExt;
        send.write_u32(bytes.len() as u32).await.unwrap();
        send.write_all(&bytes).await.unwrap();
        let ack = monocode_mobile_link::read_frame(&mut recv, RESPONSE_LIMIT).await.unwrap();
        assert_eq!(ack["watching"], true);
        let frame = monocode_mobile_link::read_frame(&mut recv, RESPONSE_LIMIT).await.unwrap();
        assert_eq!(frame["result"]["turns"][0]["items"][0]["text"], "from phone");
        // A transient polling failure must recover even at the same revision.
        fake.state.lock().unwrap().fail_method = Some("events.read".into());
        let error = tokio::time::timeout(Duration::from_secs(4), monocode_mobile_link::read_frame(&mut recv, RESPONSE_LIMIT)).await.unwrap().unwrap();
        assert!(error["result"]["error"].is_string());
        let recovered = tokio::time::timeout(Duration::from_secs(4), monocode_mobile_link::read_frame(&mut recv, RESPONSE_LIMIT)).await.unwrap().unwrap();
        assert!(recovered["result"]["error"].is_null());
        assert_eq!(recovered["result"]["busy"], true);
        // A later poll uses events.read plus sessions.sync and emits an edit.
        {
            let mut state = fake.state.lock().unwrap();
            state.snapshot["revision"] = json!(2);
            state.snapshot["session"]["blocks"][1]["text"] = json!("completed");
            state.snapshot["session"]["blocks"][1]["streaming"] = json!(false);
            state.snapshot["status"] = json!("idle");
        }
        let next = tokio::time::timeout(Duration::from_secs(4), monocode_mobile_link::read_frame(&mut recv, RESPONSE_LIMIT)).await.unwrap().unwrap();
        assert_eq!(next["result"]["busy"], false);
        assert!(fake.state.lock().unwrap().methods.iter().any(|m| m == "events.read"));
        send.finish().unwrap();
        exchange(&c, "session.cancel", "cancel", json!({"conversationId":"s"})).await.unwrap();
        exchange(&c, "session.send", "send-alias", json!({"sessionId":"s","text":"second prompt"})).await.unwrap();
        exchange(&c, "session.cancel", "cancel-again", json!({"conversationId":"s"})).await.unwrap();
        exchange(&c, "conversation.send", "send-wire", json!({"conversationId":"s","text":"third prompt"})).await.unwrap();
        let read = exchange(&c, "conversation.read", "read", json!({"conversationId":"s"})).await.unwrap();
        assert_eq!(read["turns"][0]["items"][0]["text"], "third prompt");
        exchange(&c, "device.leave", "leave", json!({})).await.unwrap();
        assert!(exchange(&c, "workspace.list", "revoked", json!({})).await.is_err());
        enrolled["deviceId"].as_str().unwrap().to_owned()
    });
    assert!(host.status().devices.is_empty());
    // Pair a second time, then revoke from the sidecar operator API.
    let join = invitation(&host);
    runtime.block_on(async {
        let (_ep, c) = client(&join, &identity, address).await;
        let result = exchange(&c, "device.enroll", "pair2", json!({"certificate":phone.certificate,"proof":phone.proof(&join.fingerprint().unwrap()).unwrap()})).await.unwrap();
        let (_ep2, c2) = client(&phone, &identity, address).await;
        host.revoke(result["deviceId"].as_str().unwrap()).unwrap();
        assert!(exchange(&c2, "workspace.list", "denied", json!({})).await.is_err());
    });
    assert!(!fs::read_to_string(settings).unwrap().contains(&phone.private_key));
    assert!(!device.is_empty());
    running.store(false, Ordering::SeqCst);
    worker.join().unwrap();
    host.shutdown();
}

#[test]
fn binary_prints_private_uri_and_qr_and_stops_cleanly() {
    use std::{io::BufRead, process::{Command, Stdio}, sync::mpsc};
    let dir = fixture();
    let token = token(dir.path());
    let fake = Fake::new(None);
    let data = dir.path().join("service-data");
    let socket = UdpSocket::bind("127.0.0.1:0").unwrap();
    let address = socket.local_addr().unwrap().to_string();
    drop(socket);
    let flags = vec!["--bind".to_owned(), address.clone(), "--token-file".into(), token.display().to_string(),
        "--data-dir".into(), data.display().to_string(), "--default-harness".into(), "codex".into(),
        "--default-model".into(), "codex:fixture".into(), "--rpc-addr".into(), fake.address.to_string()];
    let mut child = Command::new(env!("CARGO_BIN_EXE_monocode-phone-sidecar")).args(&flags).arg("--invite")
        .stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
    let out = child.stdout.take().unwrap();
    let err = child.stderr.take().unwrap();
    let (tx, rx) = mpsc::channel();
    let output = std::thread::spawn(move || {
        let mut reader = std::io::BufReader::new(out);
        let mut first = String::new();
        reader.read_line(&mut first).unwrap();
        let _ = tx.send(first.clone());
        let mut rest = String::new();
        reader.read_to_string(&mut rest).unwrap();
        first + &rest
    });
    let errors = std::thread::spawn(move || {
        let mut text = String::new();
        std::io::BufReader::new(err).read_to_string(&mut text).unwrap();
        text
    });
    let first = rx.recv_timeout(Duration::from_secs(10));
    // Always clean up the child before asserting, even if startup failed.
    unsafe { libc::kill(child.id() as i32, libc::SIGTERM); }
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() { break status; }
        if std::time::Instant::now() > deadline { child.kill().unwrap(); break child.wait().unwrap(); }
        std::thread::sleep(Duration::from_millis(20));
    };
    let stdout = output.join().unwrap();
    let stderr = errors.join().unwrap();
    assert!(first.unwrap().starts_with("monocode://pair?i="));
    assert!(status.success(), "Sidecar should handle SIGTERM without being killed");
    assert!(stdout.contains('█') || stdout.contains('▀') || stdout.contains('▄'));
    assert!(stderr.contains("router mapping disabled"));
    assert!(stderr.contains("OS keyring is not used"));
    assert!(!stdout.contains(&format!("{:_<43}", "fixture")));
    assert!(!stderr.contains(&format!("{:_<43}", "fixture")));
    assert_eq!(fs::metadata(data.join("mobile-access.json")).unwrap().permissions().mode() & 0o777, 0o600);
    let identity = storage::identity(&data).unwrap().fingerprint().unwrap();
    // Operator revocation requires neither RPC availability nor a listener.
    drop(fake);
    let revoked = Command::new(env!("CARGO_BIN_EXE_monocode-phone-sidecar")).args(&flags).args(["--revoke", "fixture-device"])
        .output().unwrap();
    assert!(revoked.status.success());
    assert_eq!(storage::identity(&data).unwrap().fingerprint().unwrap(), identity);
    let config: Value = storage::read(&data.join("mobile-access.json")).unwrap();
    assert_eq!(config["enabled"], false);
    let invalid = Command::new(env!("CARGO_BIN_EXE_monocode-phone-sidecar")).args(["--bind", "0.0.0.0:38416"]).output().unwrap();
    assert!(!invalid.status.success());
    assert!(String::from_utf8(invalid.stderr).unwrap().contains("no wildcard"));
}

#[test]
fn phone_identity_and_enrollment_survive_a_listener_restart() {
    let dir = fixture();
    let identity = storage::identity(dir.path()).unwrap();
    let phone = Identity::generate().unwrap();
    let settings = dir.path().join("mobile-access.json");
    let (host, _) = MobileHost::open_at("127.0.0.1:0".parse().unwrap(), settings.clone(), identity.clone()).unwrap();
    host.configure(true, false, "").unwrap();
    let join = invitation(&host);
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    let address = ([127,0,0,1], host.status().addresses[0].port).into();
    runtime.block_on(async {
        let (_ep, c) = client(&join, &identity, address).await;
        exchange(&c, "device.enroll", "enroll", json!({"certificate":phone.certificate,
            "proof":phone.proof(&join.fingerprint().unwrap()).unwrap()})).await.unwrap();
    });
    assert_eq!(host.status().devices.len(), 1);
    host.shutdown();
    drop(host);
    let restored = storage::identity(dir.path()).unwrap();
    assert_eq!(identity.fingerprint().unwrap(), restored.fingerprint().unwrap());
    let (host, _) = MobileHost::open_at("127.0.0.1:0".parse().unwrap(), settings, restored.clone()).unwrap();
    assert!(host.status().listening);
    assert_eq!(host.status().devices.len(), 1);
    let address = ([127,0,0,1], host.status().addresses[0].port).into();
    runtime.block_on(async {
        let (_ep, c) = client(&phone, &restored, address).await;
        let result = exchange(&c, "workspace.list", "restored", json!({})).await.unwrap();
        assert!(result["sessions"].is_array());
    });
    host.shutdown();
    // Missing identity must fail closed, not silently orphan enrolled phones.
    fs::remove_file(dir.path().join("identity.json")).unwrap();
    assert!(storage::identity(dir.path()).is_err());
}
