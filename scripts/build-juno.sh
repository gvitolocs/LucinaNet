#!/bin/bash
# Static aarch64 musl LucinaNet from the Alpine sysroot + host clang + rust-lld.
# Set WORK_DIR to a scratch directory for the sysroot, wrap scripts and build output.
# Builds a binary that runs on a Juno device; the device itself is not needed to build.
WORK_DIR=${WORK_DIR:-$(mktemp -d)}
cd "${LUCINANET_REPO:-$(cd "$(dirname "$0")/.." && pwd)}"
S=$WORK_DIR/lucinanet/sysroot
T=aarch64-unknown-linux-musl
L=$(rustc +1.99.1 --print sysroot)/lib/rustlib/x86_64-unknown-linux-gnu/bin/gcc-ld
A=$(rustc +1.99.1 --print sysroot)/lib/rustlib/x86_64-unknown-linux-gnu/bin/llvm-ar
export CARGO_TARGET_DIR=$WORK_DIR/lucinanet/target PYTHON3=python3 PYTHON=python3
W=$WORK_DIR/lucinanet/wrap; mkdir -p $W
cat > $W/cc <<W2
#!/bin/bash
args=(); for a in "\$@"; do case "\$a" in --target=*) ;; *) args+=("\$a");; esac; done
exec clang --target=aarch64-alpine-linux-musl --sysroot=$S -B$L -fuse-ld=lld "\${args[@]}"
W2
cat > $W/cxx <<W2
#!/bin/bash
args=(); for a in "\$@"; do case "\$a" in --target=*) ;; *) args+=("\$a");; esac; done
exec clang++ --target=aarch64-alpine-linux-musl --sysroot=$S -B$L -fuse-ld=lld "\${args[@]}"
W2
chmod +x $W/cc $W/cxx
export CC_aarch64_unknown_linux_musl=$W/cc CXX_aarch64_unknown_linux_musl=$W/cxx AR_aarch64_unknown_linux_musl=$A
export CARGO_TARGET_AARCH64_UNKNOWN_LINUX_MUSL_LINKER=$W/cxx
export CARGO_TARGET_AARCH64_UNKNOWN_LINUX_MUSL_RUSTFLAGS="-C target-feature=+crt-static -C link-self-contained=no -C link-arg=-static -C link-arg=-lstdc++ -L $WORK_DIR/lucinanet/wrap/lib"
export BINDGEN_EXTRA_CLANG_ARGS_aarch64_unknown_linux_musl="--target=aarch64-alpine-linux-musl --sysroot=$S"
export LIBCLANG_PATH=/usr/lib/llvm-18/lib
export AWS_LC_SYS_CMAKE_BUILDER=0
export JEMALLOC_SYS_WITH_LG_PAGE=12
export MOZJS_FROM_SOURCE=1 HOST_CC=clang HOST_CXX=clang++
exec nice -n 15 cargo build -j 6 --profile ${PROFILE:-probe} -p ${PKG:-lucinanet-servo} --target $T
