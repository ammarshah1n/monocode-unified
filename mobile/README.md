# Monocode mobile

The Expo app lives in this directory and has its own npm dependencies and lockfile. The app pairs with the desktop over QUIC and shows its real workspace. Use a Node.js LTS release for development.

## Run locally

From the repository root:

```bash
cd mobile
npm ci
npm run ios
```

`npm run ios` first builds the Rust connection library, then creates and installs the development build in an iOS simulator, then starts Metro. It requires Xcode and an installed iOS Simulator runtime (Xcode > Settings > Components). For Android, install Android Studio and start an emulator, install an Android NDK, set `ANDROID_NDK_HOME`, then use `npm run android`. Rust and rustup must be installed for both platforms. Use `npm run ios -- --device` or `npm run android -- --device` to build for a connected physical device. A physical iPhone needs Developer Mode enabled and a unique `ios.bundleIdentifier` in `app.json`.

After the first build, use `npm start` for everyday TypeScript and JavaScript changes. Press `i` or `a` in the Expo terminal to open the installed development build in a simulator or emulator. Rebuild after adding native modules, changing `app.json`, or upgrading Expo. Regenerate native folders with `npx expo prebuild --clean` before rebuilding in those cases. The generated `ios/` and `android/` folders are ignored by Git.

The routes are in `src/app/`:

- `_layout.tsx` gates the workspace behind pairing and puts it inside `SimpleDrawer` (`src/ui/SimpleDrawer.tsx`), a gesture-driven side drawer. The drawer content is `src/features/workspace/WorkspaceDrawer.tsx`, which lists monos, projects and recent sessions, with search.
- `project/[projectId]/index.tsx` is the new session screen for a project.
- `project/[projectId]/session/[sessionId].tsx` is a session transcript.
- `mono/[monoId].tsx` is a mono's conversation.

## Pair with desktop

Run the desktop app from the repository root, open **Settings → Mobile**, enable mobile access and choose **Pair device**. Scan the code in the mobile app, then choose the projects and monos to include. You can select nothing and choose later. **Mobile Settings → Projects and monos** changes the selection at any time; choices are remembered per desktop, including after reconnecting.

The host chip at the top of the workspace switches between paired desktops. **Settings → Hosts → Add host** opens the same scan/paste flow without replacing existing hosts; cancel leaves the current host unchanged. Only the active host is polled and its projects, chats, and new sessions are used. Inactive hosts show **Not connected**, not an unverified reachability claim. **Remove host** forgets that host locally (revoke this phone separately on the desktop); workspace choices and the shared phone identity are retained. Existing single-host installations migrate automatically without re-pairing.

Pairing codes expire after 15 minutes and work once. The shared Rust module uses QUIC and mutually authenticated TLS 1.3, with the desktop identity pinned by the code. Long-lived private keys are stored in the desktop OS keyring and the phone’s SecureStore. Desktop settings list paired devices and let you revoke them. Selection controls what appears on the phone; a paired device has workspace access.

Remote connections go directly to the desktop. Automatic NAT-PMP/UPnP router mapping is enabled when mobile access is first enabled. A restrictive router or carrier NAT can require a reachable VPN address or manual UDP forwarding to port **38416**. Configure the remote address before generating a code. The desktop must remain awake and running.

Workspace lists refresh while the phone is active. Opening a conversation reads its latest 200 user/assistant blocks from the desktop, with active conversation updates, text sending, starting sessions and stopping turns. The changes view reads real Git changes on demand. Agent approvals and questions still need attention on the desktop. The app retains the workspace list for offline navigation and keeps failed-send drafts; it does not queue commands offline.

The transport is an Expo local native module in `modules/monocode-link`, built by `scripts/build-native-link.mjs`. **Expo Go cannot load it**; rebuild the development app after changing native code. `npm run native:ios` and `npm run native:android` build the library separately. Generated native libraries and platform folders are ignored by Git.

## Switch provider accounts

Tap the model in the composer, then **Account**, to choose a Claude Code or Codex profile saved on your desktop. Each provider keeps its own selection. Choosing another account from an existing chat opens a new conversation with the same model and permissions; the original stays in history. Add accounts and complete sign-in in desktop **Settings → Accounts**.

The account card also shows the desktop footer's usage windows, percentages and reset times. Tap **Refresh** or pull down in the model picker to reload usage for the provider's accounts.

## EAS development builds

Run EAS commands from `mobile/`. The build input must include the root Cargo workspace and its path dependencies. The `eas-build-pre-install` hook installs the pinned Rust toolchain and builds the native transport before CocoaPods/Gradle runs.

The `development` profile builds for Android or physical iOS devices; `development-simulator` builds for the iOS simulator. EAS requires an Expo account, and physical iOS distribution requires Apple signing credentials.

```bash
npx eas-cli@latest login
npx eas-cli@latest build --platform ios --profile development-simulator
# or: npx eas-cli@latest build --platform android --profile development
```

Install the completed development build, then run `npm start` to connect it to Metro.

## Build your own iPhone app (owner steps; not performed by the coding worker)

Keep the whole repository, not just `mobile/`: the native Rust transport depends on the root Cargo workspace. Expo Go cannot run this app.

1. In `app.json`, use an iOS bundle identifier your Apple team owns and set `ios.appleTeamId` to that team's ID. The checked-in `com.hardbeat920.monocode-mobile`, Expo owner `hardbeat920`, EAS project ID, and update URL belong to the upstream project, not automatically to your account. For a separate fork, choose your own bundle identifier; remove the upstream `owner`, `extra.eas.projectId`, and `updates.url` before linking your own EAS project. Set `owner` to your Expo account/organization if needed. Changing bundle identifier creates a separate installation: automatic pairing migration requires updating the existing app with its authorized bundle ID/signing team, without uninstalling it.
2. **EAS physical iPhone build:** with your Expo account and paid Apple Developer team, run these from `mobile/` after intentionally configuring your own project:
   ```bash
   npm ci
   npx eas-cli@latest login
   npx eas-cli@latest init
   npx eas-cli@latest build:configure
   npx eas-cli@latest device:create
   npx eas-cli@latest build --platform ios --profile development
   ```
   Register the intended iPhone, select the correct Apple team, and authorize certificate/provisioning creation when prompted. Install the resulting internal development build, enable Developer Mode on the phone, then run `npm start` to serve this checkout. A simulator-only alternative is `--profile development-simulator` (no device signing). Do not submit or publish an update to the upstream project. Configure updates for your own project separately before using EAS Update.
3. **Local Mac/Xcode alternative:** install Xcode/iOS components and Rust/rustup, connect and trust the iPhone, enable Developer Mode, and sign into the correct team in Xcode. From `mobile/`, run `npm ci`, then `npm run ios -- --device`. This script builds the native Rust link before Expo prebuild/compile/install. If signing needs manual selection, open the generated `ios/*.xcworkspace` yourself, choose the app target → Signing & Capabilities → your team and owned bundle ID, choose the iPhone, then build/run. A free Personal Team may support local development subject to Apple's restrictions; it does not provide EAS ad-hoc/store distribution.

## Check the project

```bash
npx tsc --noEmit
npm run lint
```

The desktop app at the repository root remains a separate npm project.
