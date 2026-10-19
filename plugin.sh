#!/bin/sh
here="$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)"
sys="$(uname -s)"
case "$sys" in
MINGW* | MSYS* | CYGWIN*) rel="x86_64-pc-windows-msvc/gray-discord.exe" ;;
Darwin*)
    case "$(uname -m)" in
    arm64) rel="aarch64-apple-darwin/gray-discord" ;;
    *) rel="x86_64-apple-darwin/gray-discord" ;;
    esac ;;
*)
    case "$(uname -m)" in
    aarch64 | arm64) rel="aarch64-unknown-linux-musl/gray-discord" ;;
    *) rel="x86_64-unknown-linux-musl/gray-discord" ;;
    esac ;;
esac
if [ -x "$here/$rel" ]; then exec "$here/$rel" "$@"; fi
for cand in "$here/target/release/gray-discord.exe" "$here/target/release/gray-discord" "$here/target/debug/gray-discord.exe" "$here/target/debug/gray-discord"; do
    if [ -x "$cand" ]; then exec "$cand" "$@"; fi
done
echo "gray-discord binary not found in $here" >&2
exit 1
