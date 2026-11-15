#!/bin/bash
# SPDX-License-Identifier: GPL-3.0-only
# Builds the parity example program (examples/paritaet) of spektrafilm-rs in f64 (CPU) and f32 (GPU).
# Usage: WT=<working copy of the repo> build.sh      (build lock: the project's baulock.sh, if present)
: "${WT:?set WT (working copy of the graineo repo)}"
LOCK="${BAULOCK:-}"
cd "$WT/third_party/spektrafilm-rs" || exit 1
export PATH=~/.rustup/toolchains/stable-aarch64-apple-darwin/bin:$PATH
CARGO_TARGET_DIR=$WT/target $LOCK cargo build --release -j 4 -p spektrafilm-core --features precision-f64 --example paritaet 2>&1 | grep -E "^error|^warning: unused var|-->|Finished" | head -20
CARGO_TARGET_DIR=$WT/target-f32 $LOCK cargo build --release -j 4 -p spektrafilm-core --example paritaet 2>&1 | grep -E "^error|-->|Finished" | head -20
