#!/usr/bin/env bash
# Build the Rust core as shared libraries for the Android ABIs used by Gradle.
#
# Prereqs (one-time):
#   rustup target add aarch64-linux-android armv7-linux-androideabi \
#     x86_64-linux-android i686-linux-android
#   cargo install cargo-ndk
#   Install Android NDK (e.g. via Android Studio SDK Manager) and export:
#     export ANDROID_NDK_HOME=/path/to/ndk
#
# Output: copies libawareness_core.so into app/src/main/jniLibs/<abi>/.
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
OUT="$HERE/../app/src/main/jniLibs"

cargo ndk \
  -t arm64-v8a \
  -t armeabi-v7a \
  -t x86_64 \
  -o "$OUT" \
  build --release

echo "Built .so into $OUT"
