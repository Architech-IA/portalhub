#!/bin/bash
# Respaldo diario de la configuración de portalhub y de lo que lo rodea (lo que NO está en git y no se puede rehacer a mano).
#
# Se instala en /root/backup-config-portalhub.sh y corre por cron a las 04:30 UTC (el de portal-seg es a las 04:00):
#   30 4 * * * /root/backup-config-portalhub.sh >> /root/backups-config-portalhub/backup.log 2>&1
#
# Qué guarda (config_<fecha>.tgz, con rutas absolutas dentro, para restaurar con tar -C /):
#   - /opt/portalhub: extra.env (claves de Nexus/Sage), council-trigger-config.json, start.sh, start-motor.sh
#   - Nginx: portalhub-*.conf, snippets, upstream y el sitio del portal
#   - Workers del Motor: /opt/masd/worker/start*.sh y apply-firewall.sh (el firewall del usuario masd)
#   - crontab de root y el dump de pm2 (qué procesos arrancan)
#   - MANIFIESTO.txt con fechas, commits y sha256 de cada archivo
# Además conserva las 3 últimas versiones del binario /opt/portalhub/portalhub en binarios/: sirven para volver atrás en
# segundos (recompilar tarda 25-45 min).
#
# ¡Los archivos tienen CLAVES! Directorio 700 y archivos 600. Es un respaldo LOCAL (protege de errores y de un reinicio
# fallido, no de perder el servidor): si hace falta, copiarlo cifrado a otro lugar.
#
# Variables para pruebas: DEST, RETENER.
set -u
DEST="${DEST:-/root/backups-config-portalhub}"
RETENER="${RETENER:-30}"
umask 077
mkdir -p "$DEST/binarios"
chmod 700 "$DEST" "$DEST/binarios"
FECHA=$(date -u +%Y%m%d_%H%M)
TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT

CANDIDATOS=(
  /opt/portalhub/extra.env
  /opt/portalhub/council-trigger-config.json
  /opt/portalhub/start.sh
  /opt/portalhub/start-motor.sh
  /etc/nginx/portalhub-routes.conf
  /etc/nginx/portalhub-auth.conf
  /etc/nginx/conf.d/portalhub-upstream.conf
  /etc/nginx/snippets/portalhub-proxy.conf
  /etc/nginx/snippets/portalhub-proxy-largo.conf
  /etc/nginx/snippets/next-proxy.conf
  /etc/nginx/sites-enabled/portal-architechia
  /opt/masd/worker/start1.sh
  /opt/masd/worker/start2.sh
  /opt/masd/apply-firewall.sh
  /root/.pm2/dump.pm2
  /root/vigilar-portalhub.sh
  /root/backup-config-portalhub.sh
)
EXISTENTES=(); FALTAN=()
for f in "${CANDIDATOS[@]}"; do
  if [ -e "$f" ]; then EXISTENTES+=("${f#/}"); else FALTAN+=("$f"); fi
done

crontab -l > "$TMP/crontab-root.txt" 2>/dev/null || echo "(sin crontab)" > "$TMP/crontab-root.txt"
{
  echo "Respaldo de configuración de portalhub"
  echo "fecha (UTC): $(date -u '+%F %T')    servidor: $(hostname)"
  echo "commit portalhub : $(git -C /root/repos/portalhub log --oneline -1 2>/dev/null)"
  echo "commit portal    : $(git -C /root/portal-architechia log --oneline -1 2>/dev/null)"
  echo "binario          : $(sha256sum /opt/portalhub/portalhub 2>/dev/null | cut -c1-16) ($(stat -c %s /opt/portalhub/portalhub 2>/dev/null) bytes)"
  echo
  echo "sha256 de cada archivo:"
  for f in "${EXISTENTES[@]}"; do (cd / && sha256sum "$f"); done
  [ "${#FALTAN[@]}" -gt 0 ] && { echo; echo "NO existían al respaldar:"; printf '  %s\n' "${FALTAN[@]}"; }
} > "$TMP/MANIFIESTO.txt"

SALIDA="$DEST/config_$FECHA.tgz"
if ! tar -czf "$SALIDA" -C / "${EXISTENTES[@]}" -C "$TMP" crontab-root.txt MANIFIESTO.txt 2>"$TMP/tar.err"; then
  echo "[$(date -u '+%F %T')] ERROR al crear $SALIDA: $(head -c 300 "$TMP/tar.err")"
  rm -f "$SALIDA"; exit 1
fi
# verificación: el archivo se puede leer y trae el manifiesto
if ! tar -tzf "$SALIDA" 2>/dev/null | grep -q '^MANIFIESTO.txt$'; then
  echo "[$(date -u '+%F %T')] ERROR: $SALIDA quedó ilegible o incompleto"; rm -f "$SALIDA"; exit 1
fi
chmod 600 "$SALIDA"

# binario: se guarda solo si cambió (por hash) y se conservan las 3 últimas versiones
if [ -f /opt/portalhub/portalhub ]; then
  H=$(sha256sum /opt/portalhub/portalhub | cut -c1-12)
  if ! ls "$DEST/binarios"/portalhub_"$H"_* >/dev/null 2>&1; then
    cp /opt/portalhub/portalhub "$DEST/binarios/portalhub_${H}_$FECHA" && chmod 700 "$DEST/binarios/portalhub_${H}_$FECHA"
  fi
  ls -1t "$DEST/binarios"/portalhub_* 2>/dev/null | tail -n +4 | xargs -r rm -f
fi

# rotación
ls -1t "$DEST"/config_*.tgz 2>/dev/null | tail -n +$((RETENER + 1)) | xargs -r rm -f

echo "[$(date -u '+%F %T')] OK $(basename "$SALIDA") ($(stat -c %s "$SALIDA") bytes, ${#EXISTENTES[@]} archivos; faltaban ${#FALTAN[@]}); respaldos: $(ls "$DEST"/config_*.tgz | wc -l)"
