# MonoCode phone sidecar (experimental)

Standalone Fedora phone listener for the existing authenticated, loopback MonoCode
Host RPC. No Mac, desktop process, Tauri build, host source changes, router mapping,
or installed-app modifications are needed. Reuses upstream `MobileHost`, QUIC/mTLS,
Gate, enrollment proofs, session-sync/chunk decoder and incremental watch frames.

**Real iPhone / real host interoperability is UNTESTED.** Tests use disposable
loopback HTTP + QUIC fixtures only. This is not an installed or enabled service.

## Build and checks

```sh
cd /home/ammarshahin/scratch/monocode-src/crates/monocode-phone-sidecar
CARGO_BUILD_JOBS=2 cargo build --release
CARGO_BUILD_JOBS=2 cargo test -- --test-threads=2
```

This crate has its own workspace and lockfile, outside both desktop workspaces.
The binary links core, not GPUI/Tauri. Core still depends on keyring; the direct
`libdbus-sys/vendored` feature avoids needing system-wide D-Bus development headers.
No system packages are installed. Unix/Linux only.

## Trust and supported operations

- Required explicit `--bind IP:PORT` (or `MONOCODE_PHONE_BIND`). No wildcard,
  multicast or zero-port CLI listeners. Production should use the Fedora
  Tailscale IP, normally port 38416. Only the bound address is advertised.
- Optional `--models-allow PATH` / `MONOCODE_PHONE_MODELS_ALLOW` loads a text
  allow list once at startup: one exact ID per line, blank lines and `#` comments
  ignored, no wildcards. Matching ignores harness (`pi:`) and provider (`anthropic/`)
  prefixes, so bare and full catalog IDs both work across providers. The default
  model must be allowed; session start/configure reject disallowed models.
  If no catalog models match, the picker falls back to the full catalog with a
  stderr warning (command validation remains enforced). Without the flag,
  behavior is unchanged. Restart is required to load edits to the file.
- RPC defaults to `127.0.0.1:3774`; `--rpc-addr` accepts numeric loopback only.
  HTTP redirects and environment proxy discovery are disabled. Host environment
  ID is pinned on first connection; a changed host requires explicit reconnection
  with a new sidecar data directory, not silent credential reuse.
- `--token-file` / `MONOCODE_PHONE_TOKEN_FILE` is a raw 43-character bearer token
  in a user-owned, regular **0600** file, not a symlink. The token itself is never
  a flag, environment value, printed diagnostic or persisted sidecar field.
- `--data-dir` / `MONOCODE_PHONE_DATA_DIR` is an explicit, separate user-owned
  **0700** directory. Do not point it at desktop data or `~/.monocode-host`.
- Deliberately uses an isolated **0600 file identity store**, always, rather than
  probing the desktop keyring. This is reported at startup and works headlessly
  without Secret Service. `identity.json`, `mobile-access.json`, `environment.json`
  and `commands/` belong only to the sidecar. Back up this directory privately.
- A file lock excludes another sidecar using the same directory. A process-local
  umask of 0077 also protects the settings/devices written by upstream core.
- `--invite` prints a one-use, 15-minute **secret** `monocode://pair?...` URI and
  terminal QR. Use only in a private terminal; never include `--invite` in a
  service unit (it would put the invitation into journald). It invites the phone
  to the sidecar, **not** the sidecar to the existing host.
- `device.enroll`, `device.leave`, `workspace.list` and paged `sessions.list` use
  upstream MobileHost/Gate. Catalog refresh is every 3 seconds.
- Adapter allowlist: `session.start`, `session.send` (also the actual iPhone
  wire method `conversation.send`), `session.cancel`, `session.configure`,
  `models.list`, `conversation.read`, `conversation.watch`. Watch polling is every 750 ms using `events.read` and
  authoritative `sessions.sync`, including deltas/chunked transfers.
- Every other operation returns **Unsupported on this host**, including account
  usage, syntax highlighting, project mutations, Monos, approvals/questions and
  arbitrary commands. Attachments are explicitly refused.
  No approval is auto-accepted. Choose a supported provider/model explicitly via
  required `--default-harness` / `--default-model` (environment names are printed
  by `--help`); a phone's explicit start choices are passed through. The default
  runtime permission mode is **supervised**, never silently full-access.
- Only `environment.describe`, `projects.list`, `sessions.list`, `sessions.sync`,
  `sessions.syncChunk`, `events.read`, `models.list`, and `commands.dispatch` may
  reach RPC; dispatch is restricted further to `create`, `send`, `cancel`,
  `configure`.
- Host catalogs are adapted to the phone's model-sheet shape, retaining settings
  and provider names. `current` uses the session's configuration, or the sidecar's
  defaults for a new session, with advertised model-setting defaults filled in.
  Host provider-error text is not reflected to the phone.
- `session.configure` uses the real host `configure` command. Partial choices
  retain omitted fields; an explicit `modelSettings` replaces that settings map.
  Changes require an idle session. Changing its harness or provider account is
  explicitly rejected: start a new session to choose a different harness.
- Project `machine` is `Fedora`. The invitation/enrollment desktop name still
  comes from core's OS hostname. There is no working `--name` flag: changing
  those descriptors requires a core naming API, outside the allowlist-only core
  edit permission for this change.

Start persists an immutable retry record before dispatch, then uses distinct,
stable, device-scoped hashed host command IDs for **create and send**. A lost
reply/crash between phases safely reuses host receipts. Send/cancel are also
journaled; cancel pins the original run ID, so a retry cannot cancel a later run.
Configure is journaled too: its merged full configuration is frozen before dispatch,
so a lost-reply retry never re-reads or overwrites a later choice.
Reusing an ID with different input fails. Keep `commands/`: deleting retry records
weakens replay protection (especially cancels). There is no automatic journal GC.
The record includes the submitted prompt and must remain private.

Transcript conversion preserves host block/turn IDs, user text, assistant text,
streaming state, simple notices and user-turn metrics. Tools/reasoning/system
blocks are plain notices, not the full desktop folding/highlighting experience.
Approval notices tell the phone to use another host client. Reads carry the last
100 turns; watches honor `tail` and `from`. Older-history paging is unsupported.
Responses over the phone's 8 MiB limit get a clear error, not silent truncation.
Transient polling errors are sent to the watch and retried; catalog failures keep
the last catalog and report once to stderr until recovery.

## Later deployment (instructions only; not performed)

Prerequisites: an **already approved host bearer token**, the host listening on
loopback port 3774, Fedora/iPhone on the same authorized tailnet, and a provider +
model available on that host. Credential provisioning is intentionally outside
this change: no `host.mjs pair`, live pairing, host-store reads, or token extraction
were performed. Replace the source token path/model below with approved values.
Do not paste a token into shell history or place it inside a unit file.

1. Build with the commands above. Install the binary and prepare isolated state:

   ```sh
   mkdir -p "$HOME/.local/bin" "$HOME/.config" "$HOME/.local/state"
   install -d -m 700 "$HOME/.config/monocode-phone-sidecar" \
     "$HOME/.local/state/monocode-phone-sidecar"
   install -m 755 target/release/monocode-phone-sidecar "$HOME/.local/bin/monocode-phone-sidecar"
   install -m 600 /path/to/already-approved-token-file \
     "$HOME/.config/monocode-phone-sidecar/host-token"
   BIND="$(tailscale ip -4):38416"
   MODEL='provider-qualified-model-id-available-on-this-host'
   ```

   Existing `~/.local/bin` permissions are left unchanged. Data/config directories
   must be 0700.
   No firewall/ACL change is prescribed automatically; authorize the phone's
   tailnet access to this one UDP listener separately if necessary.

2. Before creating/enabling a service, run once in a **private foreground terminal**:

   ```sh
   "$HOME/.local/bin/monocode-phone-sidecar" \
     --bind "$BIND" \
     --token-file "$HOME/.config/monocode-phone-sidecar/host-token" \
     --data-dir "$HOME/.local/state/monocode-phone-sidecar" \
     --default-harness codex --default-model "$MODEL" --invite
   ```

   Scan on the real iPhone, verify projects, existing/new sessions, send, cancel
   and reconnect. Verify with the Mac off. Then Ctrl-C; keep the data directory.
   If a service already exists, stop **only the sidecar** first to release its lock.
   Never stop/reconfigure the existing host as part of this procedure.

3. **Only after approval and real-device verification**, the later user unit can
   be created at `~/.config/systemd/user/monocode-phone-sidecar.service` with the
   following content. Replace `TAILSCALE_IP` and `MODEL_ID`; these are not secrets:

   ```ini
   [Unit]
   Description=MonoCode phone sidecar
   After=network-online.target

   [Service]
   Type=simple
   ExecStart=%h/.local/bin/monocode-phone-sidecar --bind TAILSCALE_IP:38416 --token-file %h/.config/monocode-phone-sidecar/host-token --data-dir %h/.local/state/monocode-phone-sidecar --default-harness codex --default-model MODEL_ID
   Restart=on-failure
   RestartSec=5
   TimeoutStopSec=30
   UMask=0077
   NoNewPrivileges=true

   [Install]
   WantedBy=default.target
   ```

   Later activation (not run by this task):

   ```sh
   systemctl --user daemon-reload
   systemctl --user enable --now monocode-phone-sidecar.service
   systemctl --user status monocode-phone-sidecar.service
   journalctl --user -u monocode-phone-sidecar.service -n 50 --no-pager
   ```

   User services stop after logout unless user lingering is already authorized.
   Check `loginctl show-user "$USER" -p Linger`; changing lingering is a separate
   decision. No unit or linger setting was created/changed here.

4. Revocation later: stop only the sidecar unit. Read device IDs (public metadata)
   from `mobile-access.json`, e.g. `jq '.devices[] | {id,name}' ...`. Run the same
   binary/flags from step 2, replacing `--invite` with `--revoke DEVICE_ID`, then
   restart the sidecar unit. Revocation works without contacting the host and
   does not revoke/change its bearer token or any host device. `device.leave`
   also revokes the phone immediately. To issue another invite, stop the sidecar
   unit and repeat step 2 with the **same data directory**.
