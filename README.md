# portalhub

Backend JSON del portal de ArchiTechIA, en **Rust** (Axum + sqlx). Reemplazó **de a poco** las
rutas `/api/*` del portal Next.js/Prisma: hoy **toda la API** (200 rutas) la sirve Rust y Next.js
queda solo para las **páginas** (y como respaldo si portalhub cae). El frontend React **no cambia**:
sus páginas siguen llamando a las mismas URL; Nginx decide qué servicio contesta cada una.

Backlog: Solución `PHUB`, épica 0001 (portal de ArchiTechIA → Oficina/Backlog).

## Qué está migrado

| Área | Rutas | Sprint |
|---|---|---|
| Usuarios y perfil | `/api/users`, `/api/profile` | PHUB-0001-0002 |
| Calendar | `/api/meetings` y subrutas | PHUB-0001-0003 |
| Dashboard | `/api/dashboard`, `/api/dashboard/personal` | PHUB-0001-0004 |
| Leads y hub de leads | `/api/leads/**` | PHUB-0001-0005/0006 |
| Clientes, prospectos, nichos | `/api/clientes`, `/api/prospectos`, `/api/niches`, `/api/prospecting/**` | PHUB-0001-0009 |
| Backlog, notificaciones, sesiones, búsqueda, agentes (CRUD), reportes, actividades, comentarios, áreas, hitos, pipeline, propuestas | bloque 1 | PHUB-0001-0010 |
| Soluciones, iniciativas, riesgos, productos, contabilidad, RRHH, finanzas, inventario, proveedores, órdenes | bloque 2 | PHUB-0001-0010 |
| Apps, tipos de app, eventos, VPS, GitHub, métricas, resumen público, estado, Orión (bitácora y chat), Consejo SAGE, prospección con Google Places | tanda 3 | PHUB-0001-0011 |
| Proyectos (sesiones de IA, memoria, adjuntos, automatizaciones), IA de soluciones (PRD, arquitectura), chats de agentes, disparadores del Consejo, SSE | tanda 4 | PHUB-0001-0012 |
| Motor: ejecutor de tareas, worktrees de git, verificación, cierre de sprint, despliegue de proyectos, bases de datos y variables de proyecto, documentos con LibreOffice | tanda 4 (servicio privilegiado) | PHUB-0001-0012 |
| Autenticación: NextAuth v4 (credenciales + Google) y conexión con Microsoft | tanda 5 | PHUB-0001-0013 |

## Cómo está armado

Un solo binario, **dos servicios**:

```
                           ┌──────────────────────────────────────────┐
 navegador ── Nginx ──────► │ portalhub        127.0.0.1:3100  (usuario │──► Postgres (Supabase)
   │  /api/*               │ sin privilegios, 384 MB)                 │──► OpenCode, Google, GitHub…
   │                       └───────────────┬──────────────────────────┘
   │ páginas                               │ reenvía, ya autenticado, lo que toca el servidor
   ▼                                       ▼
 Next.js :3003              ┌──────────────────────────────────────────┐
 (solo páginas + respaldo)  │ portalhub-motor  127.0.0.1:3101  (root,   │──► git / worktrees / Docker / Nginx
                            │ solo con la clave interna)               │──► /root/repos, /root/deploys
                            └──────────────────────────────────────────┘
 workers Python (MASD) ──► callbacks en :3100 (clave interna) ──► el motor cierra cada tarea
```

```
src/
  main.rs        arranque: config, pool de Postgres (sesión en UTC), router (PORTALHUB_MODE=motor → servicio privilegiado)
  config.rs      variables de entorno (acepta la DATABASE_URL de Prisma tal cual)
  session.rs     cookie de NextAuth v4: valida (JWE A256GCM, HKDF-SHA256) y también la emite
  util.rs        consultas que devuelven JSON desde Postgres, ids, fechas, caché por huella
  llm.rs         cliente de OpenCode (reintentos, filtro de contenido, tool-calling)
  proxy.rs       red de seguridad: lo no portado se reenvía a Next (hoy no queda nada)
  google.rs      sincronización con Google Calendar
  extract.rs     texto de adjuntos (PDF, docx, pptx, xlsx, texto)
  texto.rs       HTML ↔ texto plano
  routes/        una pieza por área (auth.rs, proyectos.rs, council.rs, ejecutor.rs…)
  motor/         servicio privilegiado: repo.rs (git/GitHub), ejecutor.rs (ciclo de una tarea),
                 grafo.rs (dependencias entre tareas), despliegue.rs (Docker, Nginx, DB, secretos)
tools/parity/    harness de paridad Next ↔ Rust (decide cada corte): parity.js, parity2.js, t4.js, auth.js
scripts/         build.sh (compila en Docker), start.sh (público), start-motor.sh (privilegiado)
deploy/nginx/    upstream, snippets y rutas
deploy/ops/      vigilancia (cron cada minuto) y respaldo diario de la configuración
deploy/worker/   worker de Python del Motor y herramientas de los agentes (file_tools.py, con pruebas): ver deploy/worker/README.md
deploy/harness/  Harness (cola de tareas) y su API de monitoreo
```

El Motor y sus agentes: el Motor (Rust) arma el contexto, despacha a la cola del Harness y cierra la tarea (compilador `tsc` + verificador
semántico que ve el **diff real** de la tarea); los workers de Python (`deploy/worker`) llaman al modelo en un bucle de herramientas
(leer por rangos, `edit_file`, escribir en rutas permitidas, comandos en un contenedor aislado) y reportan el resultado y el uso de tokens.

### Decisiones

- **Mismo Postgres, Prisma sigue dueño del esquema.** Este servicio no crea ni altera tablas; las
  migraciones se siguen haciendo con ALTER TABLE directo desde el portal.
- **Las respuestas se arman en SQL** (`to_jsonb`, `jsonb_agg`): misma forma que Prisma sin
  declarar una struct por consulta. Las fechas se normalizan a ISO con milisegundos y `Z`.
- **La sesión es la de NextAuth** (mismas cookies y mismo JWE): las sesiones ya emitidas siguen
  valiendo y Next y Rust se entienden entre sí. Toda ruta exige sesión válida o la API key interna.
- **Dos procesos por seguridad.** Lo que necesita root (git, Docker, Nginx, secretos de despliegue)
  vive en `portalhub-motor`, que solo escucha en loopback y exige la clave interna; el servicio
  público corre sin privilegios y reenvía a él después de validar la sesión.
- **LangGraph y el CLI de `claude` ya no son necesarios.** El grafo de dependencias entre tareas es
  un planificador propio (`motor/grafo.rs`) y los disparadores del Consejo usan OpenCode por HTTP.
- **Convivencia por Nginx** con el propio Next como *backup* del upstream: si portalhub cae, la API
  vuelve sola a la implementación original (el código de Next sigue desplegado).
- **Se compila dentro de Docker**, con tope de memoria, sin instalar Rust en el VPS. Binario estático (musl).

### Diferencias deliberadas respecto de Next (correcciones)

1. **Sesión**: Next solo comprobaba que la cookie *existiera*; acá se descifra y valida. Una API sin
   sesión responde 401 JSON (Next redirigía a /login con 307).
2. **`POST /api/users`** exige ADMIN/SUPERADMIN y guarda la contraseña con bcrypt.
3. **`GET /api/profile`** ya no devuelve los tokens OAuth en crudo.
4. **Dashboard**: la caché de 30 s ya no comparte el bloque "Mi día" entre usuarios; los fines de mes
   de la tendencia se calculan bien.
5. **Diagrama generado**: un nodo en la columna/fila 0 ya no se corre a la 5 (`0 || 5` en JS).
6. **Borrado de lead** en una transacción; **borrado de reunión** lee el evento de Google antes de borrar.
7. **Consejo**: la creación de backlog/áreas desde una propuesta fallaba en Next (ids y columnas
   faltantes); acá funciona.
8. **Motor**: si falla la creación del worktree de una tarea, la tarea vuelve a BACKLOG (antes quedaba
   IN_PROGRESS para siempre).
9. **Documentos de propuesta**: `DELETE` sin `docId` ya no borra todos los documentos de la propuesta.
10. **Agentes**: la llamada al modelo manda el header `x-opencode-session` que exige la API GO.

## Operación

```bash
./scripts/build.sh                 # compila y deja el binario en /opt/portalhub/portalhub
pm2 restart portalhub              # servicio público
pm2 restart portalhub-motor        # servicio privilegiado (pm2 start /opt/portalhub/start-motor.sh --name portalhub-motor --interpreter bash)
curl -s 127.0.0.1:3100/health      # {"status":"ok","db":true}
curl -s 127.0.0.1:3101/health      # motor
```

Archivos de configuración propios (solo root): `/opt/portalhub/extra.env` (claves de Nexus/Sage),
`/opt/portalhub/council-trigger-config.json` (qué tipos de solución disparan una propuesta al crearse).

Paridad (antes de cortar una ruta):

```bash
set -a; . /root/portal-architechia/.env; set +a
node tools/parity/parity.js all       # tanda 1
node tools/parity/parity2.js all2     # bloques 1-2, tanda 3 y 4
node tools/parity/auth.js             # autenticación y sesiones intercambiables
```

### Vigilancia y respaldo

Por cron en el VPS: `deploy/ops/vigilar.sh` (cada minuto; avisa en la campana del portal a los ADMIN cuando cae o se recupera
portalhub, el Motor, Next, el sitio, el disco o un respaldo) y `deploy/ops/backup-config.sh` (diario; configuración de
portalhub, Nginx, workers y las 3 últimas versiones del binario). Detalle, pruebas y cómo restaurar: `deploy/ops/README.md`.

### Revertir

- **Un área**: añadir en `/etc/nginx/portalhub-routes.conf`, antes de `location /api/`, un
  `location ~ ^/api/<area>(/|$) { include snippets/next-proxy.conf; }` → `nginx -t && systemctl reload nginx`.
- **Solo la autenticación**: cambiar la línea activa de `/etc/nginx/portalhub-auth.conf` (las sesiones
  son intercambiables, nadie pierde la suya).
- **Todo**: comentar el `include /etc/nginx/portalhub-routes.conf;` en el sitio del portal.
- Los workers de Python tienen sus URL de callback en `/opt/masd/worker/start{1,2}.sh`
  (`EXECUTOR_*_URL`); para volver al Next original apuntarlas otra vez a `:3003`.
- No hay nada que deshacer en la base de datos.
