# portalhub

Backend JSON del portal de ArchiTechIA, en **Rust** (Axum + sqlx). Reemplaza **de a poco** las
rutas `/api/*` del portal Next.js/Prisma. El frontend React **no cambia**: sus páginas siguen
llamando a las mismas URL; Nginx decide qué servicio contesta cada una.

Backlog: Solución `PHUB`, épica 0001 (portal de ArchiTechIA → Oficina/Backlog).

## Qué está migrado

| Área | Rutas | Sprint |
|---|---|---|
| Usuarios y perfil | `/api/users`, `/api/profile` | PHUB-0001-0002 |
| Calendar | `/api/meetings`, `/{id}`, `/{id}/actions[/{actionId}]`, `/{id}/hub`, `/{id}/acta-generate` | PHUB-0001-0003 |
| Dashboard | `/api/dashboard`, `/api/dashboard/personal` | PHUB-0001-0004 |
| Leads | `/api/leads`, `/{id}`, `/{id}/notes`, `/{id}/interactions`, `/{id}/proposal`, `/{id}/backlog-items` | PHUB-0001-0005 |
| Hub de leads | `hub-file`, `hub-phase`, `architecture[/generate]`, `diagram[/generate|/chat]`, `/{id}/ai-chat` | PHUB-0001-0006 |

## Cómo está armado

```
src/
  main.rs        arranque: config, pool de Postgres (sesión en UTC), router
  config.rs      variables de entorno (acepta la DATABASE_URL de Prisma tal cual)
  session.rs     valida la cookie de NextAuth v4 (JWE A256GCM, clave HKDF-SHA256)
  util.rs        consultas que devuelven JSON desde Postgres, ids, fechas, helpers
  llm.rs         cliente de OpenCode (mismos errores que lib/opencodeChat.ts)
  google.rs      sincronización con Google Calendar
  extract.rs     texto de adjuntos (PDF, docx, pptx, xlsx, texto)
  texto.rs       HTML ↔ texto plano
  routes/        una pieza por área
tools/parity/    harness de paridad Next ↔ Rust (decide cada corte)
scripts/         build.sh (compila en Docker) y start.sh (arranque bajo pm2)
deploy/nginx/    upstream, snippet y rutas migradas
```

### Decisiones

- **Mismo Postgres, Prisma sigue dueño del esquema.** Este servicio no crea ni altera tablas; las
  migraciones se siguen haciendo con ALTER TABLE directo desde el portal.
- **Las respuestas se arman en SQL** (`to_jsonb`, `jsonb_agg`): misma forma que Prisma sin
  declarar una struct por consulta. Las fechas se normalizan a ISO con milisegundos y `Z`.
- **La sesión es la de NextAuth**: el portal sigue emitiendo la cookie y este servicio solo la
  valida (nadie se loguea dos veces). Toda ruta exige sesión válida o la API key interna.
- **Convivencia por Nginx** con el propio Next como *backup* del upstream: si portalhub cae, las
  rutas migradas vuelven solas a la implementación original.
- **Se compila dentro de Docker**, con tope de memoria, sin instalar Rust en el VPS (poco disco y
  RAM). El binario es estático (musl).
- **Límites de recursos desde el primer día**: usuario sin privilegios, `MemoryMax`, `CPUQuota` y
  `TasksMax` en el scope de systemd; escucha solo en `127.0.0.1`; recibe únicamente las 6
  variables que necesita.

### Diferencias deliberadas respecto de Next (correcciones)

1. **Sesión**: Next solo comprobaba que la cookie *existiera*; acá se descifra y valida.
2. **`POST /api/users`** exige ADMIN/SUPERADMIN y guarda la contraseña con bcrypt (antes: cualquier
   usuario logueado, contraseña en texto plano).
3. **`GET /api/profile`** ya no devuelve los tokens OAuth en crudo.
4. **Dashboard**: la caché de 30 s ya no comparte el bloque "Mi día" entre usuarios, y los fines de
   mes de la tendencia se calculan bien (JavaScript desbordaba `2026-09-31` al mes siguiente).
5. **Diagrama generado**: un nodo en la columna/fila 0 ya no se corre a la 5 (`0 || 5` en JS).
6. **Borrado de lead** en una transacción; **borrado de reunión** lee el evento de Google antes de
   borrar la fila.

## Operación

```bash
./scripts/build.sh                 # compila y deja el binario en /opt/portalhub/portalhub
pm2 restart portalhub              # (o: pm2 start /opt/portalhub/start.sh --name portalhub --interpreter bash)
curl -s 127.0.0.1:3100/health
```

Paridad (antes de cortar una ruta):

```bash
set -a; . /root/portal-architechia/.env; set +a
node tools/parity/parity.js all
```

### Revertir

- **Un área**: comentar su línea en `/etc/nginx/portalhub-routes.conf` →
  `nginx -t && systemctl reload nginx`.
- **Todo**: comentar el `include /etc/nginx/portalhub-routes.conf;` en el sitio del portal.
- No hay nada que deshacer en la base de datos ni en el código de Next (las rutas originales
  siguen ahí como respaldo).
