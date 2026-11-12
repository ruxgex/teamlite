#!/usr/bin/env bash
# Builds and installs Teamlite's call window (`teamlite-call`, embedded Chromium via CEF) into
# ~/.local/lib/teamlite/, where Teamlite finds it. Run from the repository root.
#
#   packaging/install-calls.sh            # build + install
#   PREFIX=/opt/teamlite packaging/install-calls.sh
#
# CEF (~330 MB download, ~1.5 GB unpacked) is cached in $CEF_PATH (default ~/.local/share/cef)
# and reused by later builds. Export the same CEF_PATH for `cargo build --features cef-calls`.
set -euo pipefail

export CEF_PATH="${CEF_PATH:-$HOME/.local/share/cef}"
DEST="${PREFIX:-$HOME/.local/lib/teamlite}"
cd "$(dirname "$0")/.."

# The cef crate pins a CEF build: "156.4.0+156.0.26" → CEF 156.0.26.
CEF_VERSION="$(awk '/^name = "cef"$/{getline; print}' Cargo.lock | sed -E 's/.*\+([0-9.]+)".*/\1/')"
case "$(uname -m)" in
    x86_64) PLATFORM=linux64; ARCH=x86_64 ;;
    aarch64) PLATFORM=linuxarm64; ARCH=aarch64 ;;
    *) echo "unsupported architecture: $(uname -m)" >&2; exit 1 ;;
esac

if [[ ! -f "$CEF_PATH/$CEF_VERSION/cef_linux_$ARCH/archive.json" ]]; then
    # download-cef reads CEF's whole build index, which is now over its 10 MB JSON limit. Serve it
    # a trimmed index (and the archive) from a local mirror instead; it then unpacks as usual.
    MIRROR="$(mktemp -d)"
    trap 'rm -rf "$MIRROR"; [[ -n "${SERVER:-}" ]] && kill "$SERVER" 2>/dev/null || true' EXIT
    echo "Fetching CEF $CEF_VERSION index…"
    curl -fsSL -o "$MIRROR/full.json" https://cef-builds.spotifycdn.com/index.json
    read -r FILE SHA < <(python3 -I - "$MIRROR" "$CEF_VERSION" "$PLATFORM" <<'PY'
import json, sys
d, ver, plat = sys.argv[1], sys.argv[2], sys.argv[3]
full = json.load(open(f"{d}/full.json"))
pick = lambda p: [v for v in p["versions"] if v["cef_version"].split("+")[0] == ver]
json.dump({k: {"versions": pick(p)} for k, p in full.items()}, open(f"{d}/index.json", "w"))
f = next(f for f in pick(full[plat])[0]["files"] if f["type"] == "minimal")
print(f["name"], f["sha1"])
PY
)
    echo "Downloading $FILE…"
    curl -fL --progress-bar -o "$MIRROR/$FILE" "https://cef-builds.spotifycdn.com/${FILE//+/%2B}"
    echo "$SHA  $MIRROR/$FILE" | sha1sum -c --quiet
    PORT=$(python3 -I -c 'import socket; s=socket.socket(); s.bind(("127.0.0.1",0)); print(s.getsockname()[1])')
    python3 -I -m http.server "$PORT" --bind 127.0.0.1 --directory "$MIRROR" >/dev/null 2>&1 &
    SERVER=$!
    sleep 1
    export CEF_DOWNLOAD_URL="http://127.0.0.1:$PORT"
fi

cargo build --release --features cef-calls --bin teamlite-call
rm -f "$CEF_PATH/$CEF_VERSION"/*.tar.bz2   # unpacked already
OUT=target/release

mkdir -p "$DEST/locales"
install -m755 "$OUT/teamlite-call" "$DEST/teamlite-call"
# libcef.so ships with debug symbols (1.4 GB); stripped it's ~280 MB.
strip --strip-unneeded -o "$DEST/libcef.so" "$OUT/libcef.so"
for f in libvk_swiftshader.so libvulkan.so.1 vk_swiftshader_icd.json icudtl.dat v8_context_snapshot.bin \
         resources.pak chrome_100_percent.pak chrome_200_percent.pak; do
    [[ -e "$OUT/$f" ]] && install -m644 "$OUT/$f" "$DEST/$f"
done
cp -r "$OUT/locales/." "$DEST/locales/"

echo "Installed the call window in $DEST ($(du -sh "$DEST" | cut -f1)). Restart Teamlite to use it."
