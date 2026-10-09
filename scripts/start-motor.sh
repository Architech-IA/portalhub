#!/bin/bash
# Arranque del servicio privilegiado portalhub-motor bajo pm2 (se instala en /opt/portalhub/start-motor.sh).
#
# Corre COMO ROOT (necesita git, Docker, Nginx, /root/repos, /root/worktrees, /root/deploys), pero:
# - escucha solo en 127.0.0.1:3101 (nadie de afuera lo alcanza; el servicio público lo llama con la clave interna),
# - solo hereda las variables que necesita (mismo criterio que start.sh: los secretos viajan por el entorno, nunca por argv),
# - corre dentro de un scope de systemd con tope de memoria y de procesos.
set -a
. /root/portal-architechia/.env
set +a

if [ -f /opt/portalhub/extra.env ]; then
  set -a
  . /opt/portalhub/extra.env
  set +a
fi

# Mismo pooler en modo SESIÓN que el servicio público (ver start.sh).
DATABASE_URL="${DATABASE_URL/:6543\//:5432/}"
export DATABASE_URL

# `npx tsc` (chequeo real de código) necesita Node, que vive en nvm.
NODO="$(ls -d /root/.nvm/versions/node/*/bin 2>/dev/null | head -1)"
export PATH="${NODO:+$NODO:}/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin"
export HOME=/root

NECESARIAS=" DATABASE_URL NEXTAUTH_SECRET INTERNAL_API_KEY OPENCODE_API_KEY GITHUB_TOKEN GITHUB_USERNAME GITHUB_ORG OPENCODE_EXECUTOR_MODEL OPENCODE_VERIFIER_MODEL HARNESS_API_URL SAGE_VAULT_PATH PORTAL_REPO_PATH PATH HOME "
for v in $(compgen -e); do
  case "$NECESARIAS" in
    *" $v "*) ;;
    *) unset "$v" 2>/dev/null ;;
  esac
done

export PORTALHUB_MODE=motor BIND=127.0.0.1 PORT=3101 RUST_LOG="${PORTALHUB_LOG:-info}"

exec systemd-run --scope --quiet -p MemoryMax=2G -p TasksMax=1024 -- /opt/portalhub/portalhub
