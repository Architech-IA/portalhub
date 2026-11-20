#!/bin/bash
# Compila portalhub (Rust, binario estático musl) en un contenedor de Docker y lo DESPLIEGA con comprobación y vuelta atrás.
# Sustituye en la práctica a build.sh, que instalaba el binario directo en producción sin verificar nada.
#
# Uso:  /root/repos/portalhub/scripts/desplegar.sh            (compila lo que haya en el árbol de trabajo)
#       BUILD_MEM=10g BUILD_CPUS=4 ./scripts/desplegar.sh      (ajustar recursos)
#
# 1) Compila en Docker (caché persistente en los volúmenes portalhub-cargo-registry y portalhub-target:
#    la primera vez es «en frío»; las siguientes son incrementales).
# 2) Respalda el binario actual en /opt/portalhub/portalhub.anterior e instala el nuevo con mv (atómico: los procesos
#    en marcha siguen con el viejo hasta reiniciarse).
# 3) Reinicia portalhub (público :3100) y portalhub-motor (privilegiado :3101) y espera su /health.
# 4) Si no responden bien en 60 s, devuelve el binario anterior y vuelve a reiniciarlos. Código de salida 2.
# Durante el reinicio (unos segundos) Nginx manda las rutas de la API al Next de respaldo (upstream portalhub_rs).
set -uo pipefail
cd "$(dirname "$0")/.."
LOG=/tmp/portalhub_deploy.log
JOBS="${CARGO_BUILD_JOBS:-3}"; MEM="${BUILD_MEM:-8g}"; CPUS="${BUILD_CPUS:-3}"
export NVM_DIR=/root/.nvm; . "$NVM_DIR/nvm.sh" >/dev/null 2>&1
say(){ echo "[$(date -u +%T)] $*" | tee -a "$LOG"; }
: > "$LOG"

say "compilando $(git rev-parse --short HEAD)$(git diff --quiet && git diff --cached --quiet || echo '+cambios sin commit') (jobs=$JOBS, mem=$MEM, cpus=$CPUS)"
T0=$(date +%s)
if ! docker run --rm --memory="$MEM" --memory-swap="$MEM" --cpus="$CPUS" \
    -v "$PWD":/work -w /work \
    -v portalhub-cargo-registry:/usr/local/cargo/registry \
    -v portalhub-target:/work/target \
    -e CARGO_BUILD_JOBS="$JOBS" \
    rust:1-alpine sh -c 'apk add --no-cache musl-dev >/dev/null && cargo build --release && cp target/release/portalhub /work/portalhub.bin' >> "$LOG" 2>&1; then
  say "FALLÓ la compilación — producción NO se tocó. Ver $LOG (errores al final)"; tail -25 "$LOG" | grep -E "^(error|warning: unused)|-->|^\s+\|" | head -25; exit 1
fi
say "compilado en $(( $(date +%s) - T0 )) s ($(stat -c %s portalhub.bin) bytes)"

mkdir -p /opt/portalhub
cp -p /opt/portalhub/portalhub /opt/portalhub/portalhub.anterior
install -m 755 portalhub.bin /opt/portalhub/portalhub.new && mv /opt/portalhub/portalhub.new /opt/portalhub/portalhub
say "binario instalado (el anterior quedó en /opt/portalhub/portalhub.anterior); reiniciando servicios"
./scripts/sincronizar_biblioteca.sh >> "$LOG" 2>&1 || say "aviso: no se pudo publicar la biblioteca"
pm2 restart portalhub portalhub-motor >/dev/null 2>&1

sano() { [[ "$(curl -s -m 5 http://127.0.0.1:3100/health)" == *'"db":true'* ]] && [[ "$(curl -s -m 5 http://127.0.0.1:3101/health)" == *'"status":"ok"'* ]]; }
for i in $(seq 1 30); do sano && { say "OK: portalhub y motor sanos tras ~$((i*2)) s (/health db:true)"; pm2 save >/dev/null 2>&1; exit 0; }; sleep 2; done

say "los servicios NO responden bien — VOLVIENDO al binario anterior"
mv /opt/portalhub/portalhub /opt/portalhub/portalhub.fallido
cp -p /opt/portalhub/portalhub.anterior /opt/portalhub/portalhub
./scripts/sincronizar_biblioteca.sh >> "$LOG" 2>&1 || say "aviso: no se pudo publicar la biblioteca"
pm2 restart portalhub portalhub-motor >/dev/null 2>&1
for i in $(seq 1 30); do sano && break; sleep 2; done
say "revertido; estado: $(curl -s -m 5 http://127.0.0.1:3100/health) | $(curl -s -m 5 http://127.0.0.1:3101/health). Binario fallido en /opt/portalhub/portalhub.fallido"
exit 2
