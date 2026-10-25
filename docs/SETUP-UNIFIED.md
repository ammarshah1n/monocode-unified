# Ammar's MonoCode setup — one project list, any machine, your phone

This branch adds to upstream MonoCode:

- **One project per folder** across machines, plus a **Host chip** beside "Full access" in the composer (This Mac / the other machine) to choose where a new session runs.
- **Split-screen chats from different folders** (same drag as before).
- **Quieter CPU/RAM/power**: cheaper idle animation, lighter chat saving.
- **iPhone app** with the same merged project list and a Host picker, pairing to each host with a QR code.
- **`crates/monocode-phone-sidecar`**: a small Linux service that lets the phone connect to an always-on Linux machine.

What was verified: Mac + Linux (Fedora) host + iPhone. A Windows PC as a host is **not verified** (see the end).

## Paste this into your Codex or Claude Code agent

```text
Set up MonoCode from this repository on my devices by following docs/SETUP.md
step by step. First ask me which devices I have (Mac, Linux machine, Windows PC,
iPhone), which is always on, and whether Tailscale and SSH keys already exist.
Check what is already installed before changing anything and skip finished steps.
Run commands yourself; only stop for things only I can do: signing in to
Tailscale/accounts, entering passwords, accepting the TestFlight invite, scanning
QR codes on my phone. Never print or store tokens, keys or invite URLs outside
the terminal. Ask before installing system packages. After each part, verify it
with the check listed in the doc and tell me the result.
```

## 0. Plan and prerequisites

Pick the **always-on machine** (the "main host"). Every device joins one Tailscale network so they can reach each other without port forwarding.

1. Install Tailscale on every device and sign in to the **same** account. `tailscale ip -4` prints a device's address (100.x.y.z).
2. SSH between computers (for the app to set up a remote machine): on the Mac, `ssh-keygen -t ed25519` if you have no key, then `ssh-copy-id USER@MAIN_HOST_TAILSCALE_IP`. Test `ssh USER@MAIN_HOST_TAILSCALE_IP true`. Linux needs `openssh-server` running (`sudo systemctl enable --now sshd`).
3. Install and sign in to at least one agent (Claude Code, Codex, ...) on **each host that will run sessions**. See `docs/providers.md`.

## 1. Mac app (no Xcode needed)

Needs Node 22+, Rust nightly (`rustup toolchain install nightly`) and Apple Command Line Tools (`xcode-select --install`).

```sh
git clone <this repo> monocode && cd monocode
git checkout share-unified
export RUSTUP_TOOLCHAIN=nightly CARGO_BUILD_JOBS=2 CARGO_INCREMENTAL=0 CC=/usr/bin/clang CXX=/usr/bin/clang++
npm install
node scripts/build-native.mjs        # ~15–30 min the first time
```

Result: `crates/monocode-gpui/target/release/Monocode Native.app` (ad-hoc signed; it never needs notarising on your own Mac). Copy it to `~/Applications` and open it.

- First launch asks to use a Keychain item: click **Always Allow**. A local rebuild changes the signature, so expect this prompt again after each rebuild. If the app opens but shows no window, a Keychain prompt is waiting — find it and approve it.
- Check: the app opens and you can start a session.

## 2. Connect the Mac to the Linux host (this installs the host)

In the Mac app: add a remote machine over SSH using the Linux machine's Tailscale IP. The app installs and starts `monocode-host` there (needs `curl`, `tar`, `sha256sum` on the Linux machine). Check: the Linux machine's projects appear in the Mac app and the Host chip offers both machines.
If you prefer the terminal on the Linux machine: install Node 24+, then `monocode-host service install` (the host listens on `127.0.0.1:3774`; `monocode-host status` checks it).

## 3. Linux phone sidecar (the phone's endpoint)

Install the host, then the phone sidecar.

```sh
# phone sidecar
cd monocode/crates/monocode-phone-sidecar
CARGO_BUILD_JOBS=2 cargo build --release
install -Dm755 target/release/monocode-phone-sidecar ~/.local/bin/monocode-phone-sidecar
```

The sidecar needs a bearer token issued by the local host. Issue one (it is shown once) and store it privately — type or paste it at the hidden prompt so it never lands in shell history:

```sh
install -d -m 700 ~/.config/monocode-phone-sidecar ~/.local/state/monocode-phone-sidecar
~/.monocode-host/bin/monocode-host pair --name phone-sidecar   # copy the token it prints
umask 077; read -rs T; printf %s "$T" > ~/.config/monocode-phone-sidecar/host-token; unset T
```

Optionally restrict models with `--models-allow FILE` (one exact model ID per line).

Systemd user service (replace `TS_IP` with `tailscale ip -4`, and the default model with one your agent offers):

```ini
# ~/.config/systemd/user/monocode-phone-sidecar.service
[Unit]
Description=MonoCode phone sidecar
After=network-online.target

[Service]
Type=simple
ExecStart=%h/.local/bin/monocode-phone-sidecar --bind TS_IP:38416 --token-file %h/.config/monocode-phone-sidecar/host-token --data-dir %h/.local/state/monocode-phone-sidecar --default-harness claude --default-model claude-sonnet-5-5
Restart=on-failure
RestartSec=5
UMask=0077
NoNewPrivileges=true

[Install]
WantedBy=default.target
```

```sh
systemctl --user daemon-reload && systemctl --user enable --now monocode-phone-sidecar
loginctl enable-linger "$USER"      # keep it running when you are logged out
systemctl --user is-active monocode-phone-sidecar
```

The sidecar talks to the MonoCode host on `127.0.0.1:3774`, so the host from step 2 must be running on this machine.

## 4. iPhone

1. Send the maintainer your Apple ID email; they invite you to TestFlight. Install **TestFlight** from the App Store, accept the invite, install **Monocode**.
2. Install Tailscale on the phone and sign in to the same account (turn the VPN on).
3. **Pair with the Mac:** in the Mac app choose **Pair phone**; scan the QR in the phone app (Add host).
4. **Pair with the Linux machine:** the invite process needs the data directory, so pause the service, make the invite, and start the service again after the phone has paired:
   ```sh
   systemctl --user stop monocode-phone-sidecar
   monocode-phone-sidecar --token-file ~/.config/monocode-phone-sidecar/host-token \
     --data-dir ~/.local/state/monocode-phone-sidecar --bind TS_IP:38416 \
     --default-harness claude --default-model claude-sonnet-5-5 --invite
   ```
   A QR code prints in the terminal; scan it within 15 minutes. When the phone shows the host, press Ctrl-C and run `systemctl --user start monocode-phone-sidecar`.
5. Check: in the phone app, pick the Linux host and start a session.

## 5. Windows gaming PC

- The MonoCode desktop app installs on Windows and can connect to hosts. The host CLI lists Windows, but the install script only handles Linux and macOS. To use the PC as a host, the safest route is Linux inside WSL2 (steps 2–3 there). **Untested.**
- Put Tailscale on it so the Mac and phone can reach it.

## Troubleshooting

- App has no window after launch: approve the pending Keychain prompt.
- Phone cannot connect: Tailscale on, same account, sidecar active, port 38416/UDP reachable on the Tailscale IP.
- Pairing QR expired: make a new one (15-minute lifetime).
- Keep the main host awake (disable sleep) and logged in, or enable linger as above.
