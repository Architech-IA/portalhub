# Worker del Motor Agéntico y herramientas de los agentes

Código que corre en el VPS **fuera de este repo** y que ahora queda versionado aquí. Los scripts de arranque
(`/opt/masd/worker/start1.sh`, `start2.sh`) llevan claves y **no** se versionan.

| Archivo | Va en | Qué es |
|---|---|---|
| `masd_worker.py` | `/opt/masd/worker/` | Worker: toma tareas de la cola del Harness, llama al modelo (OpenCode) en un bucle de herramientas y reporta a portalhub |
| `file_tools.py` | `/opt/masd/worker/` | Herramientas de archivo de los agentes: `list_files`, `read_file`, `grep_files`, `write_file`, `edit_file`, `delete_file`, `move_file`, `run_command` |
| `code_index.py` | `/opt/masd/worker/` | Índice de símbolos (`find_symbol`, `get_file_summary`, `get_dependents`) |
| `harness.py` | `/opt/masd/worker/` | Copia del Harness que ve el worker (usa la cola montada en `/opt/masd/harness-queue`) |
| `test_file_tools.py` | `/opt/masd/worker/` | 28 pruebas de `file_tools.py` |
| `../harness/harness.py`, `harness_api.py` | `/root/` | Harness (cola de tareas) y su API de monitoreo (:8767) |

Instalar o actualizar el worker:

```bash
install -o masd -g masd -m 640 deploy/worker/{masd_worker.py,file_tools.py,code_index.py,test_file_tools.py} /opt/masd/worker/
(cd /opt/masd/worker && python3 -m unittest test_file_tools)     # 28 pruebas
pm2 restart masd-worker-1 masd-worker-2                          # solo si no hay tareas en curso
```

## Qué pueden hacer los agentes (fase 2, 09/10/2026)

- **Leer** (`read_file`): sin rango devuelve el principio (~6.000 caracteres) y avisa cuántas líneas tiene; con `start_line`/`end_line` devuelve ese trozo
  (hasta ~12.000 caracteres). Pueden leer `package.json`, el esquema de Prisma y `next.config`; no `.env`, `.git`, `.github`, lockfiles ni la config de pm2.
- **Cambiar** (`edit_file`): reemplaza un trozo **exacto** (único, o `replace_all`) sin reescribir el archivo; respeta CRLF. `write_file` crea archivos o los
  reemplaza enteros, pero **rechaza** una reescritura que pese menos de la mitad de un archivo grande (parece truncada) salvo `reemplazar_completo=true`.
  `delete_file` y `move_file` para refactors y migraciones.
- **Dónde escriben**: `src/`, `app/`, `pages/`, `components/`, `lib/`, `tests/`, `test/`, `__tests__/`, `docs/`, `public/`, `scripts/`, `prisma/migrations/` y
  `README.md`, `Dockerfile`, `.dockerignore`. Nunca `.env`, `.git`, `.github` (los workflows de CI corren con secretos al hacer push), `node_modules`,
  lockfiles ni la política. `package.json` se cambia con `npm install`.
- **Política por repositorio** (`.masd-policy.json` en la raíz, escrita por una persona; el agente no puede tocarla):
  `{"escritura": ["prisma/schema.prisma", "next.config.js", "backend/"]}` amplía lo anterior con esas rutas (exactas o carpetas con `/`). No desbloquea
  secretos, `.git`, `.github` ni la propia política. Los repos **nuevos** de proyecto la reciben automática (`motor/repo.rs::politica_por_defecto`: `next.config.*` y
  el esquema de Prisma). El repo del portal **no** tiene política: ahí esos archivos siguen protegidos.
- **Comandos** (`run_command`, en un contenedor aislado): `npm install|ci|init|run <script>` (no `dev/start/serve/watch`), `npx create-next-app|create-vite|create-react-app|tsc`
  y, nuevo, `npx prisma validate|format|generate` (no tocan ninguna base de datos).
- **Medición**: el worker manda al Motor el uso de tokens de la tarea (`usage`: entrada, salida, total, llamadas, modelo) y portalhub lo guarda en
  `TaskExecution.artifacts.usage` y lo narra en la traza.
- **Verificación**: el Motor corre `tsc --noEmit` cuando la tarea tocó archivos `.ts/.tsx` con `write_file`, `edit_file` o `move_file` (se ignoran los intentos rechazados).

Límites que siguen vigentes: 20 rondas de herramientas y 8.000 tokens por llamada; solo Node/TypeScript; sin ejecutar la aplicación.

## Seguridad pendiente

El worker confía en lo que trae la tarea de la cola (`apiUrl`, `repoPath`, prompts) y la API del Harness (`:8767`) escucha en todas las interfaces sin autenticación.
Ver `deploy/ops/README.md` y el backlog (sprint PHUB-0001-0015).
