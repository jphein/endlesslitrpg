#!/usr/bin/env bash
# Build and test on `familiar` instead of locally.
#
# familiar has 24 threads to katana's 8, and runs the same toolchain (1.97.1), so a
# cold workspace build is roughly three times faster. It is also where the daemon is
# meant to run, so the toolchain being there is useful beyond compile speed.
#
#   tools/build-on-familiar.sh test --workspace
#   tools/build-on-familiar.sh clippy --workspace --all-targets -- -D warnings
#   tools/build-on-familiar.sh build -p litrpg-engine --features sherpa
#
# Source is pushed with rsync each run, so what builds is what is on disk here.
# `target/` stays on familiar and is never copied back: the point is to avoid
# shipping gigabytes, and a binary built there is what you want to *run* there.
#
# Nothing is deleted on familiar that is not part of the source tree — `--delete` is
# scoped by the excludes below, so the remote `target/` cache survives between runs
# and incremental builds stay fast.
set -euo pipefail

REMOTE=${LITRPG_BUILD_HOST:-familiar}
REMOTE_DIR=${LITRPG_BUILD_DIR:-/home/jp/build/endlesslitrpg}
LOCAL_DIR=$(cd "$(dirname "$0")/.." && pwd)

if [ $# -eq 0 ]; then
  echo "usage: $(basename "$0") <cargo-subcommand> [args...]" >&2
  exit 64
fi

# Excludes matter for correctness, not just speed:
#   target/  — 3+ GB, and the remote has its own cache
#   data/ media/ models/ — the live story and 500 MB of TTS models; a build must
#                          never depend on them, so not copying them proves it
rsync -az --delete \
  --exclude 'target/' \
  --exclude 'data/' \
  --exclude 'media/' \
  --exclude 'models/' \
  --exclude '.git/' \
  "$LOCAL_DIR/" "$REMOTE:$REMOTE_DIR/"

# `~/.cargo/bin` is not on a non-interactive PATH on either machine — the same trap
# that made cargo look absent on familiar in the first place.
#
# LD_LIBRARY_PATH points at the remote target dir so a `--features sherpa` binary can
# find libsherpa-onnx-c-api.so, which `download-binaries` drops there.
ssh "$REMOTE" "
  export PATH=\"\$HOME/.cargo/bin:\$PATH\"
  export LD_LIBRARY_PATH=\"$REMOTE_DIR/target/debug:\${LD_LIBRARY_PATH:-}\"
  cd '$REMOTE_DIR'
  exec cargo $*
"
