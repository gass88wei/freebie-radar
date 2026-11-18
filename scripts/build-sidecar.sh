#!/usr/bin/env bash
# 构建桌面版 Tauri 侧车（Node.js 24 LTS linux-x64 单文件）
# 产物：src-tauri/binaries/node-x86_64-unknown-linux-gnu
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
DIST_BASE="${NODE_DIST_BASE:-https://nodejs.org/dist}"
NODE_VERSION="${NODE_VERSION:-}"
ARCHIVE=""
TARGET_DIR="$REPO_ROOT/src-tauri/binaries"
TARGET="$TARGET_DIR/node-x86_64-unknown-linux-gnu"

log() { printf '[build-sidecar] %s\n' "$*"; }
die() { printf '[build-sidecar] 错误：%s\n' "$*" >&2; exit 1; }

fetch() {
  if command -v curl >/dev/null 2>&1; then
    curl -fsSL "$1"
  else
    wget -qO- "$1"
  fi
}

download() {
  local url="$1" out="$2"
  if command -v aria2c >/dev/null 2>&1; then
    aria2c -c -x8 -s8 -k1M --file-allocation=none --summary-interval=0 \
      -d "$(dirname "$out")" -o "$(basename "$out")" "$url"
  else
    wget -c -O "$out" "$url"
  fi
}

# 未显式指定版本时，从 latest-v24.x 的校验和清单解析最新 24 LTS。
# 注意：SHASUMS256.txt 第一行是 aix-ppc64 之类的其它平台，必须挑 linux-x64 那一行。
if [[ -z "$NODE_VERSION" ]]; then
  log "解析 Node.js 24 LTS 最新版本..."
  shasums="$(fetch "$DIST_BASE/latest-v24.x/SHASUMS256.txt")" || die "无法获取版本清单"
  NODE_VERSION="$(printf '%s\n' "$shasums" \
    | awk '$2 ~ /-linux-x64\.tar\.xz$/ {print $2; exit}' \
    | sed -e 's|^node-v||' -e 's|-linux-x64\.tar\.xz$||')"
  [[ -n "$NODE_VERSION" ]] || die "版本解析失败，请用 NODE_VERSION=x.y.z 指定"
fi

ARCHIVE="node-v${NODE_VERSION}-linux-x64.tar.xz"
EXTRACT_DIR="node-v${NODE_VERSION}-linux-x64"
BASE_URL="$DIST_BASE/v${NODE_VERSION}"

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

log "下载 $ARCHIVE ..."
download "$BASE_URL/$ARCHIVE" "$TMP/$ARCHIVE" || die "下载失败：$ARCHIVE"

log "校验 sha256 ..."
shasums_file="$TMP/SHASUMS256.txt"
fetch "$DIST_BASE/v${NODE_VERSION}/SHASUMS256.txt" > "$shasums_file" \
  || die "无法获取 SHASUMS256.txt"
grep "  $ARCHIVE\$" "$shasums_file" > "$TMP/expected.sha256" \
  || die "校验和清单中未找到 $ARCHIVE"
(cd "$TMP" && sha256sum -c expected.sha256 >/dev/null) \
  || die "sha256 校验失败：$ARCHIVE"

log "解压 node ..."
tar -xJf "$TMP/$ARCHIVE" -C "$TMP"
[[ -x "$TMP/$EXTRACT_DIR/node" ]] || die "未找到解压后的 node 二进制"

mkdir -p "$TARGET_DIR"
cp "$TMP/$EXTRACT_DIR/node" "$TARGET"
chmod 755 "$TARGET"

log "完成：$TARGET"
"$TARGET" --version
