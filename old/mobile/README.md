# Threadlane mobile client (iOS)

A thin GPUI client (gpui-mobile + gpui-kit) that connects to the Threadlane
desktop app over the shared daemon WebSocket contract. Browse projects and
Git summaries, search chats, start a chat, stream transcripts, send or queue
messages, and answer permission and question prompts. The composer offers
models from the desktop catalog, supported reasoning efforts, and Agent/Fusion
mode. Session options include refresh, new chat in the project, and archive.

New chats begin as drafts and persist on the first accepted prompt. New-chat
creation and composer options require desktop protocol version 4 or newer;
older desktops show an update message. Fusion mode follows the desktop's
project setting.

## Pairing

1. Desktop: sidebar footer → **Share with mobile** (phone icon) →
   **Start sharing**. The dialog shows a QR code encoding
   `threadlane://pair?host=…&port=…&token=…` plus the same link as text.
2. iPhone: scan the QR with the Camera app — the registered
   `threadlane://` URL scheme launches the app and auto-fills host, port,
   and token.
3. The desktop's listener binds all IPv4 interfaces (`0.0.0.0`) and requires
   a pairing token. Paired devices retain their credentials for reconnection.
   Treat the QR/link as a credential; remove a paired device on the desktop
   to revoke its access.

### Connecting over Tailscale

1. Connect the Mac and iPhone to the same Tailscale network (tailnet).
2. Enable **Share with mobile** on the Mac; use **Add device** for a new
   pairing invitation. Keep Threadlane running and the Mac awake and online.
3. In the iPhone's **Connection details**, set **Host** to the Mac's Tailscale
   **IPv4 address**, for example `100.101.102.103`, without a URL prefix.
   Keep the port and token from the pairing invitation, or your existing
   saved connection. Tap **Connect**.
4. To verify access across networks, turn off Wi-Fi on the iPhone and
   reconnect over cellular with Tailscale still connected.

The desktop QR/link normally advertises its LAN address, not its Tailscale
address. After scanning, change **Host** before connecting over Tailscale.
MagicDNS hostnames are not accepted by the pairing client; use the numeric
IPv4 address. No router port forwarding, exit node, Serve, or Funnel is needed.
If blocked, check the Mac's firewall, Tailscale incoming-connection setting,
and tailnet access rules for the pairing port.

Pairing accepts Tailscale's shared IPv4 range (`100.64.0.0/10`) in addition
to local-network addresses. That range is also used by ISP carrier-grade NAT:
an address alone does **not** prove that a connection is encrypted. Keep
Tailscale connected and use the address assigned to your Mac in its device
list. Tailscale encrypts the VPN traffic; Threadlane's `ws://` pairing transport
does not add TLS. Sharing still listens on LAN interfaces too, so keep tokens
private and do not expose the sharing port publicly.

## Layout

- `rust/` — `threadlane-mobile` crate: `staticlib` + `rlib`, its own
  Cargo workspace using the same GPUI 0.3.8 API as desktop. `client.rs`
  adapts the shared `threadlane-client` transport to the iOS executor;
  `app.rs` owns pairing and mobile navigation. Shared state, session cards,
  transcripts, and composer surfaces live in the client and UI leaf crates.
- `ios/` — XcodeGen container: `App.swift` embeds the GPUI surface and
  forwards deep links; `project.yml` builds `libthreadlane_mobile.a` in
  a pre-build script.

## Build & run (simulator)

```bash
rustup target add aarch64-apple-ios-sim
brew install xcodegen
cd ios && ./build.sh            # or: ./build.sh "iPhone 17"
```

## Build & run (physical iPhone over USB)

1. Add an Apple ID in Xcode → Settings → Accounts — a free "Personal
   Team" is enough for dev installs.
2. Plug the iPhone in over USB and trust the Mac on the phone; enable
   Developer Mode on the phone (Settings → Privacy & Security →
   Developer Mode) if it is not already on.
3. Run:

```bash
cd ios
DEVELOPMENT_TEAM=D9MPPGN38L ./build.sh device            # first connected iPhone
DEVELOPMENT_TEAM=D9MPPGN38L ./build.sh device "Sab's iPhone"
```

`DEVELOPMENT_TEAM` is your ten-character team id (Apple Developer
account → Membership, or the Personal Team Xcode created). The script
cross-compiles the Rust staticlib for `aarch64-apple-ios`, builds the
Release app, signs it, and installs + launches via `devicectl`.
Device builds allow Xcode to contact Apple to create or update signing
assets and register the destination device with the selected team. The
Apple account in Xcode must have permission to manage those assets.
If provisioning fails, verify the team selection and account permissions
in Xcode; an installed distribution certificate alone is not enough for
a development install.

On-device pairing works like the simulator path: scan the desktop's QR
with the Camera app while both devices share the same LAN.

Same-machine testing: when the desktop app and the simulator run on the
same Mac, the pairing QR encodes the LAN address, which the simulator
reaches directly — no special config needed.

## Diagnosing device startup failures

A successful build/install and `Launched application` message do not prove
that the app stayed alive. List connected devices with `xcrun devicectl list devices`,
then replace `DEVICE_ID` in these commands with your phone's identifier:

```bash
xcrun devicectl device info processes --device DEVICE_ID --search Threadlane
xcrun devicectl device process launch --device DEVICE_ID --terminate-existing --console --timeout 30 dev.threadlane.mobile
xcrun devicectl device info files --device DEVICE_ID --domain-type systemCrashLogs --search Threadlane
```

The console command restarts the app and waits for exit, bounded to 30 seconds;
a timeout alone is not evidence of a crash. UIKit crash details may be absent
from the console even when the launch command reports exit code 0. Copy a
specific report returned by the last command for its exception and stack:

```bash
xcrun devicectl device copy from --device DEVICE_ID --domain-type systemCrashLogs --source REPORT_NAME.ips --destination /tmp/threadlane-mobile-crash.ips
```

The Swift host uses a single `UIWindowScene` and `SceneDelegate`. Keep window
creation, activation callbacks, and cold/warm pairing URL delivery on that
lifecycle: iOS 27 device logs showed a startup `SIGTRAP` in
`UIApplicationEvaluateRuntimeIssueForNoSceneLifecycleAdoption` with the old
app-delegate-only host.
