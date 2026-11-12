#!/bin/sh
# Builds the plugin to wasm, then wyrcan with the plugin embedded. Needs: rustup target add wasm32-wasip1
set -e
cd "$(dirname "$0")"
(cd plugin && cargo build --release --target wasm32-wasip1)
cargo build --release
