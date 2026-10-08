#!/bin/bash
# Compila portalhub DENTRO de un contenedor (no se instala Rust en el VPS) y deja el binario
# estático (musl) en /opt/portalhub/portalhub.
#
# El VPS comparte RAM con el portal y los demás proyectos: el contenedor de compilación tiene un
# tope de memoria, así que si la compilación se pasa, muere el contenedor de build y no la
# producción. La caché de dependencias y de compilación vive en volúmenes de Docker
# (portalhub-cargo-registry / portalhub-target), por eso las compilaciones siguientes son rápidas.
set -euo pipefail
cd "$(dirname "$0")/.."

JOBS="${CARGO_BUILD_JOBS:-2}"
MEM="${BUILD_MEM:-1400m}"

docker run --rm --memory="$MEM" --memory-swap="$MEM" --cpus=2 \
  -v "$PWD":/work -w /work \
  -v portalhub-cargo-registry:/usr/local/cargo/registry \
  -v portalhub-target:/work/target \
  -e CARGO_BUILD_JOBS="$JOBS" \
  rust:1-alpine sh -c 'apk add --no-cache musl-dev >/dev/null && cargo build --release && cp target/release/portalhub /work/portalhub.bin'

mkdir -p /opt/portalhub
install -m 755 portalhub.bin /opt/portalhub/portalhub.new
mv /opt/portalhub/portalhub.new /opt/portalhub/portalhub
echo "OK: /opt/portalhub/portalhub ($(stat -c %s /opt/portalhub/portalhub) bytes)"
