use monocode_core::mobile::{Event, MobileHost};
use monocode_phone_sidecar::{storage, Bridge, Rpc};
use qrcode::{render::unicode, QrCode};
use std::{net::SocketAddr, path::PathBuf, sync::{atomic::{AtomicBool, Ordering}, mpsc, Arc}, time::Duration};

const HELP: &str = "monocode-phone-sidecar --bind IP:PORT --token-file PATH --data-dir PATH
  --default-harness PROVIDER --default-model MODEL [--rpc-addr 127.0.0.1:3774]
  [--models-allow PATH] [--invite] [--revoke DEVICE_ID]
Environment alternatives: MONOCODE_PHONE_BIND, MONOCODE_PHONE_TOKEN_FILE,
MONOCODE_PHONE_DATA_DIR, MONOCODE_PHONE_DEFAULT_HARNESS, MONOCODE_PHONE_DEFAULT_MODEL,
MONOCODE_PHONE_MODELS_ALLOW (optional).
No wildcard binds, router mapping, pairing with the host, or host-store access.
--invite prints a secret, 15-minute pairing URI and QR; use only in a private terminal.
--revoke removes a phone while this process owns the sidecar data directory, then exits.";

struct Args {
    bind: SocketAddr,
    token: PathBuf,
    data: PathBuf,
    harness: String,
    model: String,
    rpc: SocketAddr,
    invite: bool,
    revoke: Option<String>,
    models_allow: Option<PathBuf>,
}
impl Args {
    fn parse() -> Result<Option<Self>, String> {
        let mut values = std::collections::HashMap::new();
        let mut invite = false;
        let mut revoke = None;
        let mut args = std::env::args().skip(1);
        while let Some(key) = args.next() {
            match key.as_str() {
                "--help" | "-h" => { println!("{HELP}"); return Ok(None); }
                "--invite" => invite = true,
                "--revoke" => revoke = Some(args.next().ok_or("Missing revoke device ID")?),
                "--bind" | "--token-file" | "--data-dir" | "--default-harness" | "--default-model" | "--rpc-addr" | "--models-allow" => {
                    let value = args.next().ok_or_else(|| format!("Missing value for {key}"))?;
                    if values.insert(key, value).is_some() { return Err("Duplicate option".into()); }
                }
                _ => return Err("Unknown option; use --help".into()),
            }
        }
        let get = |flag: &str, env: &str| values.get(flag).cloned().or_else(|| std::env::var(env).ok()).ok_or_else(|| format!("Supply {flag} or {env}"));
        let bind: SocketAddr = get("--bind", "MONOCODE_PHONE_BIND")?.parse().map_err(|_| "Invalid bind address; use numeric IP:PORT")?;
        let ip = bind.ip().to_canonical();
        if ip.is_unspecified() || ip.is_multicast() || bind.port() == 0
            || matches!(ip, std::net::IpAddr::V4(v) if v.is_broadcast())
        {
            return Err("Supply an explicit unicast bind IP and nonzero port (no wildcard)".into());
        }
        let rpc = values.get("--rpc-addr").map(String::as_str).unwrap_or("127.0.0.1:3774").parse().map_err(|_| "Invalid RPC address")?;
        Ok(Some(Self { bind, rpc, token: get("--token-file", "MONOCODE_PHONE_TOKEN_FILE")?.into(),
            data: get("--data-dir", "MONOCODE_PHONE_DATA_DIR")?.into(),
            harness: get("--default-harness", "MONOCODE_PHONE_DEFAULT_HARNESS")?,
            model: get("--default-model", "MONOCODE_PHONE_DEFAULT_MODEL")?, invite, revoke,
            models_allow: values.get("--models-allow").cloned().or_else(|| std::env::var("MONOCODE_PHONE_MODELS_ALLOW").ok()).map(PathBuf::from) }))
    }
}

fn run(args: Args) -> Result<(), String> {
    // Process-local only: settings/devices written by the upstream host stay private too.
    unsafe { libc::umask(0o077); }
    storage::directory(&args.data)?;
    let _lock = storage::lock(&args.data)?;
    let identity = storage::identity(&args.data)?;
    eprintln!("Phone identity: private file store in the sidecar data directory; OS keyring is not used.");
    let settings = args.data.join("mobile-access.json");
    // Revocation is offline: don't bind or require a working host credential.
    if let Some(device) = args.revoke {
        // Disable only this private sidecar config before opening it.
        if settings.exists() {
            let mut config: serde_json::Value = storage::read(&settings)?;
            config["enabled"] = false.into();
            storage::write(&settings, &config)?;
        }
        let (host, _) = MobileHost::open_at(args.bind, settings, identity)?;
        host.revoke(&device)?;
        host.shutdown();
        eprintln!("Phone revoked from sidecar. Restart sidecar to resume access.");
        return Ok(());
    }
    let rpc = Rpc::connect(args.rpc, &args.token, &args.data)?;
    let bridge = Bridge::new(rpc, &args.data, args.harness, args.model)?;
    let bridge = match args.models_allow { Some(path) => bridge.with_models_allow(&path)?, None => bridge };
    let initial = bridge.catalog()?;
    let (host, events) = MobileHost::open_at(args.bind, settings, identity)?;
    host.publish(initial)?;
    host.configure(true, false, "")?;
    if !host.status().listening { return Err("Phone listener did not start".into()); }
    eprintln!("Phone listener: {} (router mapping disabled)", args.bind);
    let running = Arc::new(AtomicBool::new(true));
    let signals = running.clone();
    let (ready_tx, ready_rx) = mpsc::sync_channel(1);
    let signal_thread = std::thread::spawn(move || {
        let result = (|| -> Result<(), String> {
            let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().map_err(|e| e.to_string())?;
            runtime.block_on(async {
                let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).map_err(|e| e.to_string())?;
                let mut interrupt = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt()).map_err(|e| e.to_string())?;
                let _ = ready_tx.send(());
                tokio::select! { _ = interrupt.recv() => (), _ = term.recv() => () }
                Ok::<_, String>(())
            })
        })();
        if result.is_err() { eprintln!("Cannot install shutdown signal handler; stopping sidecar"); }
        signals.store(false, Ordering::SeqCst);
    });
    ready_rx.recv().map_err(|_| "Cannot install shutdown signal handler")?;
    if args.invite {
        let invite = host.invite()?;
        let qr = QrCode::new(invite.url.as_bytes()).map_err(|_| "Cannot encode invitation QR")?;
        println!("{}\n{}\nExpires at Unix time {}", invite.url,
            qr.render::<unicode::Dense1x2>().quiet_zone(true).build(), invite.expires);
    }
    // Polling/catalog I/O never holds up a phone's command response.
    let watching = running.clone();
    let watch_host = host.clone();
    let mut poller = bridge.clone();
    let (watch_tx, watch_rx) = mpsc::sync_channel(64);
    let watch_thread = std::thread::spawn(move || {
        while watching.load(Ordering::SeqCst) {
            while let Ok(event) = watch_rx.try_recv() {
                match event {
                    Event::Watch(start) => {
                        let result = poller.watch(start.id.clone(), start.conversation_id.clone(), start.tail, start.from.clone(), start.incremental);
                        start.accept(result);
                    }
                    Event::WatchStop(id) => poller.stop_watch(&id),
                    Event::Request(_) => (),
                }
            }
            poller.poll(&watch_host);
            std::thread::sleep(Duration::from_millis(50));
        }
    });
    let publishing = running.clone();
    let publish_host = host.clone();
    let catalog = bridge.clone();
    let catalog_thread = std::thread::spawn(move || {
        let mut failed = false;
        while publishing.load(Ordering::SeqCst) {
            for _ in 0..30 {
                if !publishing.load(Ordering::SeqCst) { return; }
                std::thread::sleep(Duration::from_millis(100));
            }
            let result = catalog.catalog().and_then(|value| publish_host.publish(value));
            if result.is_err() && !failed { eprintln!("Host catalog refresh failed; retaining last catalog, retrying"); }
            failed = result.is_err();
        }
    });
    while running.load(Ordering::SeqCst) {
        match events.try_recv() {
            Ok(Event::Request(request)) => {
                let result = bridge.request(&request.command_id, &request.method, &request.params);
                request.reply(result);
            }
            Ok(event) => {
                if let Err(mpsc::TrySendError::Full(Event::Watch(start))) = watch_tx.try_send(event) {
                    start.accept(Err("Host is busy; retry watch".into()));
                }
            }
            Err(_) => std::thread::sleep(Duration::from_millis(20)),
        }
    }
    host.shutdown();
    let _ = signal_thread.join();
    let _ = watch_thread.join();
    let _ = catalog_thread.join();
    Ok(())
}
fn main() {
    let result = Args::parse().and_then(|args| match args { Some(args) => run(args), None => Ok(()) });
    if let Err(error) = result { eprintln!("{error}"); std::process::exit(1); }
}
