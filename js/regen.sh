#!/bin/bash
# Rebuilds the generated bindings from the bindgen output and the functions in src/engine.
cd "$(dirname "$0")/tools/gen-jsapi" && CARGO_TARGET_DIR=${WORK_DIR:-$(mktemp -d)}/lucinanet/js-engine/target-gen cargo run -q --release -- ../../mozjs-sys/bindgen ../../mozjs-sys/src/engine ../../mozjs-sys/src
