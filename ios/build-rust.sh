#!/bin/bash
# Builds CleanWeather's Rust core for iOS (device and simulator) into
# ios/build/CleanWeatherCore.xcframework, and generates its Swift bindings into
# ios/Generated/. Run on a Mac with Xcode and rustup:
#
#   ios/build-rust.sh            # release build
#   ios/build-rust.sh --debug    # faster to build, slower to run
#
# The Xcode project runs this before every build (it's quick when nothing
# changed), so usually there's no need to run it by hand.
#
# No API keys are built in: the app works with OPERA's public radar data out of
# the box, and keys are entered in its Settings (kept in the keychain).
set -euo pipefail

cd "$(dirname "$0")/.."
ROOT="$PWD"
OUT="$ROOT/ios/build"
GEN="$ROOT/ios/Generated"

PROFILE=release
FLAG=--release
if [[ "${1:-}" == "--debug" ]]; then
    PROFILE=debug
    FLAG=
fi

# Xcode runs build phases with a minimal PATH: find cargo and rustup.
export PATH="$HOME/.cargo/bin:/opt/homebrew/bin:/usr/local/bin:$PATH"
# Xcode's own settings for the app build mustn't leak into Rust's builds
# (they'd point the host build at the iOS SDK).
unset SDKROOT PLATFORM_NAME IPHONEOS_DEPLOYMENT_TARGET LLVM_TARGET_TRIPLE_SUFFIX

export IPHONEOS_DEPLOYMENT_TARGET=17.0
# The desktop app's build script embeds local API keys; nothing here should.
export CLEANWEATHER_NO_EMBED=1

DEVICE=aarch64-apple-ios
SIMS=(aarch64-apple-ios-sim x86_64-apple-ios)

rustup target add "$DEVICE" "${SIMS[@]}" >/dev/null

for target in "$DEVICE" "${SIMS[@]}"; do
    cargo build -p cleanweather-ffi --lib $FLAG --target "$target"
done

# The Swift bindings come from the library's own metadata (a build for this
# Mac is the simplest to read it from).
MAC_SDK="$(xcrun --sdk macosx --show-sdk-path)"
SDKROOT="$MAC_SDK" cargo build -p cleanweather-ffi --lib $FLAG
HOST_LIB="$ROOT/target/$PROFILE/libcleanweather_ffi.dylib"
mkdir -p "$GEN"
SDKROOT="$MAC_SDK" cargo run -q -p cleanweather-ffi --features bindgen --bin uniffi-bindgen -- \
    generate --library "$HOST_LIB" --language swift --out-dir "$GEN"

# Headers and module map for the XCFramework.
rm -rf "$OUT/headers" "$OUT/sim" "$OUT/CleanWeatherCore.xcframework"
mkdir -p "$OUT/headers" "$OUT/sim"
cp "$GEN/cleanweather_ffiFFI.h" "$OUT/headers/"
cp "$GEN/cleanweather_ffiFFI.modulemap" "$OUT/headers/module.modulemap"

# One simulator library for both Apple silicon and Intel Macs.
lipo -create \
    "$ROOT/target/${SIMS[0]}/$PROFILE/libcleanweather_ffi.a" \
    "$ROOT/target/${SIMS[1]}/$PROFILE/libcleanweather_ffi.a" \
    -output "$OUT/sim/libcleanweather_ffi.a"

xcodebuild -create-xcframework \
    -library "$ROOT/target/$DEVICE/$PROFILE/libcleanweather_ffi.a" -headers "$OUT/headers" \
    -library "$OUT/sim/libcleanweather_ffi.a" -headers "$OUT/headers" \
    -output "$OUT/CleanWeatherCore.xcframework" >/dev/null

echo "Built $OUT/CleanWeatherCore.xcframework and $GEN/cleanweather_ffi.swift"
