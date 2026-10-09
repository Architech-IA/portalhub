#!/bin/bash
# Vigila portalhub y lo que lo rodea, y avisa en la campana del portal cuando algo cae o se recupera.
#
# Se instala en /root/vigilar-portalhub.sh y corre cada minuto por cron:
#   * * * * * /root/vigilar-portalhub.sh
#
# Reglas:
#  - Un chequeo se da por caído tras 2 fallas seguidas (~2 min): un reinicio o un despliegue (segundos) no dispara alarma.
#  - Avisa UNA vez al caer y UNA vez al recuperarse (no cada minuto).
#  - El aviso es una Notification para los usuarios ADMIN y SUPERADMIN (campana del portal) y una línea en el log.
#    Si la base de datos no responde no se puede escribir la notificación: queda solo el log.
#  - Que Next conteste NO prueba que Rust esté vivo (Nginx lo usa de respaldo en silencio), por eso se mira cada servicio directo.
#
# Variables para pruebas (por defecto, producción): DIR, URL_PUBLICO, URL_MOTOR, URL_NEXT, URL_SITIO, PREFIJO.
set -u
DIR="${DIR:-/var/lib/portalhub-vigilancia}"
LOG="${LOG:-/var/log/portalhub-vigilancia.log}"
URL_PUBLICO="${URL_PUBLICO:-http://127.0.0.1:3100}"
URL_MOTOR="${URL_MOTOR:-http://127.0.0.1:3101}"
URL_NEXT="${URL_NEXT:-http://127.0.0.1:3003}"
URL_SITIO="${URL_SITIO:-https://portal.architechia.co}"
PREFIJO="${PREFIJO:-}"
mkdir -p "$DIR"
exec 9>"$DIR/lock"
flock -n 9 || exit 0

log() { echo "[$(date -u '+%F %T')] $*" >> "$LOG"; }

# avisar <tipo: error|info> <título> <mensaje>
avisar() {
  local url
  url=$( set -a; . /root/portal-architechia/.env 2>/dev/null; echo "${DATABASE_URL%%\?*}" )
  [ -z "$url" ] && { log "sin DATABASE_URL; aviso solo en el log: $2"; return; }
  PGCONNECT_TIMEOUT=8 psql "$url" -q -v ON_ERROR_STOP=1 -v t="$1" -v ti="${PREFIJO}$2" -v m="$3" >/dev/null 2>&1 <<'SQL' \
    || log "no pude escribir la notificación en la base: $2"
INSERT INTO "Notification" (id, "userId", type, title, message, link, read, "createdAt")
SELECT gen_random_uuid()::text, id, :'t', :'ti', :'m', '/dashboard', false, NOW()
FROM "User" WHERE role IN ('SUPERADMIN', 'ADMIN');
SQL
}

# verificar <id> <nombre> <qué pasa si cae> <comando…>   (el comando devuelve 0 si todo está bien)
verificar() {
  local id="$1" nombre="$2" detalle="$3"; shift 3
  local f="$DIR/$id" fallas=0 avisado=0
  [ -f "$f" ] && read -r fallas avisado < "$f"
  if "$@"; then
    if [ "${avisado:-0}" = 1 ]; then
      log "RECUPERADO: $nombre"
      avisar info "Recuperado: $nombre" "$nombre volvió a funcionar con normalidad."
    fi
    echo "0 0" > "$f"
  else
    fallas=$(( ${fallas:-0} + 1 ))
    if [ "$fallas" -ge 2 ] && [ "${avisado:-0}" != 1 ]; then
      log "CAÍDO: $nombre — $detalle"
      avisar error "Alerta: $nombre" "$detalle"
      avisado=1
    fi
    echo "$fallas $avisado" > "$f"
  fi
}

c_publico() { curl -fsS -m 5 "$URL_PUBLICO/health" 2>/dev/null | grep -q '"status":"ok"'; }
c_base()    { curl -fsS -m 5 "$URL_PUBLICO/health" 2>/dev/null | grep -q '"db":true'; }
c_motor()   { curl -fsS -m 5 "$URL_MOTOR/health" 2>/dev/null | grep -q '"status":"ok"'; }
c_next()    { [ "$(curl -s -m 8 -o /dev/null -w '%{http_code}' "$URL_NEXT/login")" = 200 ]; }
c_sitio()   { [ "$(curl -s -m 10 -o /dev/null -w '%{http_code}' "$URL_SITIO/login")" = 200 ]; }
c_disco()   { [ "$(df --output=pcent / | tail -1 | tr -dc 0-9)" -lt 85 ]; }
c_resp_bd() { find /root/backups -name 'backup_*.dump' -mmin -1560 2>/dev/null | grep -q .; }
c_resp_cfg(){ find /root/backups-config-portalhub -name 'config_*.tgz' -mmin -1560 2>/dev/null | grep -q .; }

verificar publico  "portalhub (la API en Rust)"        "El servicio portalhub (puerto 3100) no responde. Nginx está mandando la API a Next como respaldo, sin la mejora de velocidad. Revisar: pm2 status portalhub; pm2 logs portalhub." c_publico
verificar base     "base de datos vista desde portalhub" "portalhub responde pero no logra hablar con la base de datos (Supabase). La API puede fallar." c_base
verificar motor    "portalhub-motor (ejecutor de tareas)" "El Motor (puerto 3101) no responde: no se podrán despachar ni cerrar tareas. Revisar: pm2 status portalhub-motor; pm2 logs portalhub-motor." c_motor
verificar next     "portal Next (páginas y respaldo)"   "Next (puerto 3003) no responde: las páginas del portal están caídas y no hay respaldo de la API. Revisar: pm2 status portal-architechia." c_next
verificar sitio    "sitio portal.architechia.co"        "El sitio público no devuelve 200 en /login (Nginx, certificado HTTPS o Next). Revisar: nginx -t; systemctl status nginx." c_sitio
verificar disco    "disco del servidor"                 "El disco del servidor pasó del 85 %. Compilaciones, Docker y respaldos pueden fallar. Revisar: df -h; docker system df." c_disco
verificar resp_bd  "respaldo diario de la base"         "No hay un respaldo de la base de datos de las últimas 26 horas en /root/backups. Revisar el cron de /root/backup-db.sh y /root/backups/backup.log." c_resp_bd
verificar resp_cfg "respaldo de la configuración"       "No hay un respaldo de configuración de las últimas 26 horas en /root/backups-config-portalhub. Revisar /root/backups-config-portalhub/backup.log." c_resp_cfg
exit 0
