#!/usr/bin/env bash
# Build + run the Threadlane mobile client on an iOS simulator or a
# USB-connected iPhone.
#
# Usage:
#   ./build.sh                       # first available iPhone simulator
#   ./build.sh "iPhone 17"           # named simulator
#   ./build.sh device                # physical iPhone over USB
#   ./build.sh device "Sab's iPhone" # a specific connected device
#
# Device builds need code signing:
#   DEVELOPMENT_TEAM=ABCDE12345 ./build.sh device
# A free Apple ID works: add it in Xcode -> Settings -> Accounts, keep
# "Automatically manage signing" and the personal team is picked up from
# DEVELOPMENT_TEAM automatically.
set -euo pipefail

cd "$(dirname "$0")"

RUST_DIR="../rust"
TARGET_SIM="aarch64-apple-ios-sim"
TARGET_DEV="aarch64-apple-ios"
BUNDLE_ID="dev.threadlane.mobile"
APP_NAME="ThreadlaneMobile"

MODE="sim"
if [ "${1:-}" = "device" ]; then
    MODE="device"
    shift
fi
DEVICE="${1:-}"

if [ "$MODE" = "device" ]; then
    RUST_TARGET="$TARGET_DEV"
    CARGO_FLAGS="--release"
    CONFIGURATION="Release"
    PLATFORM_DIR="iphoneos"
    if [ -z "${DEVELOPMENT_TEAM:-}" ]; then
        echo "Device builds need signing: DEVELOPMENT_TEAM=<team-id> ./build.sh device" >&2
        echo "Find it at https://developer.apple.com/account (Membership Team ID)" >&2
        exit 1
    fi
else
    RUST_TARGET="$TARGET_SIM"
    CARGO_FLAGS=""
    CONFIGURATION="Debug"
    PLATFORM_DIR="iphonesimulator"
fi

# ── 1. Rust static library ────────────────────────────────────────────────────
rustup target add "$RUST_TARGET" >/dev/null 2>&1 || true
echo "==> cargo build --target $RUST_TARGET $CARGO_FLAGS"
cargo build --manifest-path "$RUST_DIR/Cargo.toml" --lib --target "$RUST_TARGET" $CARGO_FLAGS

# ── 2. Xcode project ──────────────────────────────────────────────────────────
if ! command -v xcodegen >/dev/null 2>&1; then
    echo "xcodegen not found — brew install xcodegen" >&2
    exit 1
fi
xcodegen

# ── 3. xcodebuild ─────────────────────────────────────────────────────────────
EXTRA_SETTINGS=()
if [ "$MODE" = "sim" ]; then
    DESTINATION="platform=iOS Simulator"
    [ -n "$DEVICE" ] && DESTINATION="platform=iOS Simulator,name=$DEVICE"
else
    EXTRA_SETTINGS+=(
        "DEVELOPMENT_TEAM=$DEVELOPMENT_TEAM"
        -allowProvisioningUpdates
        -allowProvisioningDeviceRegistration
    )
    DESTINATION="platform=iOS"
    if [ -n "$DEVICE" ]; then
        # Resolve a device name to its identifier for a stable destination.
        UDID=$(xcrun devicectl list devices --json-output /tmp/devicectl.json >/dev/null 2>&1 \
            && python3 -c \
                "import json; d=json.load(open('/tmp/devicectl.json')); print(next((x['identifier'] for x in d['result']['devices'] if x['hardwareProperties']['marketingName']==\"$DEVICE\" or x['deviceProperties']['name']==\"$DEVICE\"), ''))")
        [ -n "$UDID" ] && DESTINATION="platform=iOS,id=$UDID" || DESTINATION="platform=iOS,name=$DEVICE"
    fi
fi
echo "==> xcodebuild ($DESTINATION, $CONFIGURATION)"
xcodebuild \
    -project "$APP_NAME.xcodeproj" \
    -scheme "$APP_NAME" \
    -destination "$DESTINATION" \
    -configuration "$CONFIGURATION" \
    "${EXTRA_SETTINGS[@]}" \
    build | tail -20

# ── 4. Install + launch ───────────────────────────────────────────────────────
APP_PATH=$(find ~/Library/Developer/Xcode/DerivedData \
    -name "$APP_NAME.app" -path "*$CONFIGURATION-$PLATFORM_DIR*" -newer project.yml \
    | head -1)
if [ -z "$APP_PATH" ]; then
    echo "Could not locate built $APP_NAME.app under DerivedData" >&2
    exit 1
fi

if [ "$MODE" = "device" ]; then
    # Pick the USB device: explicit name/UDID, else the first physical one.
    xcrun devicectl list devices --json-output /tmp/devicectl.json >/dev/null
    TARGET_UDID=$(python3 - "$DEVICE" <<'EOF'
import json, sys
want = sys.argv[1]
devs = json.load(open("/tmp/devicectl.json"))["result"]["devices"]
# devicectl marks simulators via 'simulated' reality; keep physical devices.
phys = [d for d in devs
        if d.get("hardwareProperties", {}).get("reality") != "simulated"
        and d.get("connectionProperties", {}).get("transportType") != "sameMachine"]
name = lambda d: d.get("deviceProperties", {}).get("name", "?")
if want:
    hit = next((d for d in phys if want in (d["identifier"],)
                or want.lower() in name(d).lower()), None)
    if not hit:
        names = ", ".join(name(d) for d in phys) or "none"
        sys.exit(f"device '{want}' not found (connected: {names})")
    print(hit["identifier"])
else:
    if not phys:
        sys.exit("no physical device connected — plug in an iPhone over USB")
    print(phys[0]["identifier"])
EOF
)
    xcrun devicectl device install app --device "$TARGET_UDID" "$APP_PATH"
    xcrun devicectl device process launch --device "$TARGET_UDID" "$BUNDLE_ID"
    echo "==> Launched $BUNDLE_ID on $TARGET_UDID"
    exit 0
fi

if [ -z "$DEVICE" ]; then
    BOOTED=$(xcrun simctl list devices booted -j | python3 -c \
        'import json,sys; d=json.load(sys.stdin); print(next((u for devs in d["devices"].values() for u in devs if u["state"]=="Booted"), ""))')
    if [ -z "$BOOTED" ]; then
        # Boot the first iPhone we can find.
        BOOTED=$(xcrun simctl list devices available -j | python3 -c \
            'import json,sys; d=json.load(sys.stdin); print(next(u["udid"] for rt,devs in d["devices"].items() if "iPhone" in rt for u in devs))')
        xcrun simctl boot "$BOOTED"
        open -a Simulator
    fi
else
    BOOTED=$(xcrun simctl list devices available -j | python3 -c \
        "import json,sys; d=json.load(sys.stdin); print(next(u[\"udid\"] for devs in d[\"devices\"].values() for u in devs if u[\"name\"]==\"$DEVICE\"))")
    xcrun simctl boot "$BOOTED" 2>/dev/null || true
    open -a Simulator
fi

xcrun simctl install "$BOOTED" "$APP_PATH"
xcrun simctl launch "$BOOTED" "$BUNDLE_ID"
echo "==> Launched $BUNDLE_ID on $BOOTED"
