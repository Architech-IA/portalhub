# Vigilancia y respaldo de portalhub

Dos scripts que corren en el VPS por cron (instalados en `/root`, igual que los demás respaldos del servidor):

| Script (en el repo) | Se instala en | Cron | Para qué |
|---|---|---|---|
| `vigilar.sh` | `/root/vigilar-portalhub.sh` | cada minuto | Avisa en la campana del portal cuando algo cae o se recupera |
| `backup-config.sh` | `/root/backup-config-portalhub.sh` | 04:30 UTC | Respalda la configuración que no está en git |

Instalar o actualizar:

```bash
install -m 700 deploy/ops/vigilar.sh       /root/vigilar-portalhub.sh
install -m 700 deploy/ops/backup-config.sh /root/backup-config-portalhub.sh
# crontab -e:
#   * * * * * /root/vigilar-portalhub.sh
#   30 4 * * * /root/backup-config-portalhub.sh >> /root/backups-config-portalhub/backup.log 2>&1
```

## Vigilancia (`vigilar.sh`)

Chequea cada minuto, cada servicio **directo** (que Next conteste no prueba que Rust esté vivo: Nginx lo usa de respaldo en silencio):

| Chequeo | Qué mira | Si cae |
|---|---|---|
| `publico` | `127.0.0.1:3100/health` (portalhub) | La API sigue por Next, sin la mejora de velocidad |
| `base` | `"db":true` en ese mismo `/health` | La API puede fallar |
| `motor` | `127.0.0.1:3101/health` | No se despachan ni cierran tareas |
| `next` | `127.0.0.1:3003/login` | Páginas caídas y sin respaldo de la API |
| `sitio` | `https://portal.architechia.co/login` | Nginx, certificado HTTPS o Next |
| `disco` | `/` por debajo del 85 % | Compilaciones, Docker y respaldos pueden fallar |
| `resp_bd` | hay un `backup_*.dump` de menos de 26 h en `/root/backups` | El respaldo diario de la base dejó de correr |
| `resp_cfg` | hay un `config_*.tgz` de menos de 26 h | El respaldo de configuración dejó de correr |

Reglas:

- Cae tras **2 fallas seguidas** (~2 min): un reinicio o un despliegue (segundos) no dispara alarma.
- Avisa **una vez** al caer y **una vez** al recuperarse.
- El aviso es una `Notification` para los usuarios `ADMIN` y `SUPERADMIN` (campana del portal, link `/dashboard`) y una línea en `/var/log/portalhub-vigilancia.log`.
- **Si la base de datos está caída no se puede escribir la notificación**: queda solo en el log.
- El estado vive en `/var/lib/portalhub-vigilancia/<chequeo>` (`fallas avisado`). Borrar el archivo reinicia ese chequeo.

Probar sin tocar producción (estado, log y título aparte; luego borrar las notificaciones `[PRUEBA]`):

```bash
export DIR=/tmp/vig LOG=/tmp/vig.log PREFIJO='[PRUEBA] '
URL_PUBLICO=http://127.0.0.1:3199 /root/vigilar-portalhub.sh   # 2 veces → alerta
/root/vigilar-portalhub.sh                                     # sano → "Recuperado"
```

Para sumar un chequeo: una función `c_algo()` que devuelva 0 si todo está bien y una línea `verificar id "nombre" "qué pasa si cae" c_algo`.

## Respaldo (`backup-config.sh`)

Guarda en `/root/backups-config-portalhub/` (directorio 700, archivos 600) un `config_<fecha>.tgz` por día (se conservan 30) con:

- `/opt/portalhub`: `extra.env` (claves de Nexus/Sage), `council-trigger-config.json`, `start.sh`, `start-motor.sh`
- Nginx: `portalhub-*.conf`, snippets (`portalhub-proxy`, `portalhub-proxy-largo`, `next-proxy`), upstream y el sitio del portal
- Workers del Motor: `/opt/masd/worker/start{1,2}.sh` y `/opt/masd/apply-firewall.sh`
- el crontab de root y `~/.pm2/dump.pm2`
- `MANIFIESTO.txt`: fecha, commits y sha256 de cada archivo

Además `binarios/` conserva las **3 últimas versiones del binario** (por hash): volver atrás toma segundos; recompilar, 25-45 min.

**Tiene claves.** Es un respaldo **local**: protege de un error o de un reinicio fallido, no de perder el servidor. Si hace falta, copiarlo cifrado a otro lugar.

### Restaurar

```bash
ls -1t /root/backups-config-portalhub/config_*.tgz | head            # elegir uno
mkdir /tmp/restaurar && tar -xzf <archivo> -C /tmp/restaurar
cat /tmp/restaurar/MANIFIESTO.txt                                    # qué trae y con qué hash
cp -p /tmp/restaurar/opt/portalhub/extra.env /opt/portalhub/         # solo lo que haga falta
nginx -t && systemctl reload nginx                                   # si se tocó Nginx
pm2 restart portalhub portalhub-motor
```

Volver a un binario anterior:

```bash
ls -1t /root/backups-config-portalhub/binarios/
install -m 755 /root/backups-config-portalhub/binarios/portalhub_<hash>_<fecha> /opt/portalhub/portalhub
pm2 restart portalhub portalhub-motor
```

Comprobado el 09/10/2026: se restauró un respaldo en una carpeta temporal y los 17 archivos coincidieron byte a byte con producción.
