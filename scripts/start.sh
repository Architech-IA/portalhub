#!/bin/bash
# Arranque de portalhub bajo pm2 (se instala en /opt/portalhub/start.sh, dueño root).
#
# SECRETOS: viajan SOLO por el entorno heredado del proceso, NUNCA como argumentos de un
# comando (un `env VAR=valor ...` queda visible para cualquier usuario del servidor en `ps`,
# en /proc/<pid>/cmdline y en la descripción del scope de systemd). Un primer intento de este
# script lo hacía así y exponía las credenciales: no volver a ese patrón.
#
# - Lee el .env del portal (solo root puede) y descarta TODO el entorno salvo las variables que
#   el servicio necesita: no hereda GITHUB_TOKEN, claves de otros modelos ni nada más.
# - Corre como el usuario sin privilegios `portalhub`, dentro de un scope de systemd con tope de
#   memoria/CPU/procesos (cierra MASD-0023-0006-031 para este servicio).
# - Escucha solo en 127.0.0.1: el único camino desde internet es Nginx.
set -a
. /root/portal-architechia/.env
set +a

# Claves de los agentes Hermes (Nexus / Sage) y otros ajustes propios de portalhub: no están en el .env
# del portal. Archivo solo legible por root.
if [ -f /opt/portalhub/extra.env ]; then
  set -a
  . /opt/portalhub/extra.env
  set +a
fi

# La base está detrás del pooler de Supabase. Prisma usa el puerto 6543 (modo TRANSACCIÓN, con
# `?pgbouncer=true`); el driver de Rust (sqlx) hace dos viajes por consulta y en ese modo cada
# viaje puede caer en una conexión distinta ("unnamed prepared statement does not exist"). El
# mismo host expone el modo SESIÓN en el 5432: cada conexión del servicio es suya de punta a
# punta. Solo se cambia el puerto; usuario, host y contraseña quedan como están en el .env.
DATABASE_URL="${DATABASE_URL/:6543\//:5432/}"
export DATABASE_URL

NECESARIAS=" DATABASE_URL NEXTAUTH_SECRET INTERNAL_API_KEY OPENCODE_API_KEY GOOGLE_CLIENT_ID GOOGLE_CLIENT_SECRET GITHUB_TOKEN GITHUB_USERNAME VPS_METRICS_URL VPS_METRICS_TOKEN VPS2_METRICS_URL VPS2_METRICS_TOKEN GOOGLE_PLACES_API_KEY OPENCODE_EXECUTOR_MODEL NEXUS_PORTAL_KEY SAGE_PORTAL_KEY HERMES_HOST COUNCIL_TRIGGER_CONFIG MOTOR_URL NEXTAUTH_URL MICROSOFT_CLIENT_ID MICROSOFT_CLIENT_SECRET MICROSOFT_REDIRECT_URI MICROSOFT_TOKEN_ENCRYPTION_KEY PATH "
for v in $(compgen -e); do
  case "$NECESARIAS" in
    *" $v "*) ;;
    *) unset "$v" 2>/dev/null ;;
  esac
done

export BIND=127.0.0.1 PORT=3100 RUST_LOG="${PORTALHUB_LOG:-info}"

exec systemd-run --scope --quiet --uid=portalhub --gid=portalhub \
  -p MemoryMax=384M -p MemorySwapMax=0 -p CPUQuota=80% -p TasksMax=256 -- \
  /opt/portalhub/portalhub
