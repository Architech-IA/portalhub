# Motor de fases (MASD-0024)

Conduce un proyecto desde que es un lead hasta la entrega, sobre la **misma Solución**: 6 fases de preventa
(las etapas del lead) y 6 de ejecución. Una demo o MVP construido en preventa es la base del proyecto si la
venta se gana: no hay copia, la Solución y su repositorio siguen siendo los mismos.

## Fases (plantilla `plantillas/proyecto_completo.json`)

| # | Fase | Bloque | Estado del lead |
|---|------|--------|-----------------|
| 1 | Identificación | Preventa | NEW |
| 2 | Contacto | Preventa | CONTACTED |
| 3 | Diagnóstico | Preventa | DIAGNOSIS |
| 4 | Demo / MVP | Preventa | DEMO_VALIDATION |
| 5 | Propuesta | Preventa | PROPOSAL_SENT |
| 6 | Negociación (puerta de **resultado**) | Preventa | NEGOTIATION |
| 7 | Arranque | Ejecución | RESULT + WON |
| 8 | Diseño y plan | Ejecución | — |
| 9 | Construcción | Ejecución | — |
| 10 | QA y aceptación | Ejecución | — |
| 11 | Despliegue | Ejecución | — |
| 12 | Entrega y cierre | Ejecución | — |

Cada fase define: objetivo, **actividades** (de tipo `AGENTE` o `HUMANA`, con área y tipo de tarea del Backlog),
**entregables** y una **puerta** (aprobador y criterios). Al iniciar un proyecto la plantilla se **copia** a
`ProyectoFase.definicion`: cambiar el JSON después no altera proyectos en curso.

## Reglas

- Al entrar a una fase se crean sus actividades como tareas del Backlog (`[Fase N · Nombre] título`, estado
  `BACKLOG`, sin sprint, con su área). Las de agente NO se disparan solas: se ejecutan desde Oficina. Se crean una
  sola vez (`FaseActividad` tiene único `(solucionId, fase, clave)`).
- Para pasar a la siguiente fase hay que marcar todos los criterios de la puerta (quién y cuándo quedan
  guardados). Un SUPERADMIN puede saltárselos con `forzar` (queda anotado en el historial).
- Mover fases, marcar criterios e iniciar es solo para ADMIN/SUPERADMIN (o la API key interna).
- **Lead ↔ fase en ambos sentidos.** Editar el lead (`PUT /api/leads/{id}`) mueve la fase
  (`fases::tras_actualizar_lead`); avanzar o retroceder en preventa pone el estado del lead. Un cambio del lead
  no mueve un proyecto que ya está en ejecución.
- **Puerta de Negociación.** `GANADO` convierte la preventa en proyecto: el PRD vigente se guarda como versión 1
  en `SolucionPrdVersion`, el lead queda `RESULT/WON` con `solucionAsociada = 'Project'`, la Solución pasa a
  tipo `PROJECT` (nombre `"{empresa} — Project"`), los sprints existentes se marcan `metadata.origen = "PREVENTA"`
  (para medir cuánto costó vender) y el proyecto entra a la fase Arranque. `PERDIDO` exige motivo, cierra el lead
  y el proyecto queda `CERRADO_PERDIDO`.
- Retroceder exige motivo, reinicia los criterios de las fases que se repiten y no cruza de ejecución a preventa.
- Se notifica a los administradores (campana del portal) cuando se confirma o se pierde una venta y cuando se
  completa el proyecto.
- Una Solución sin lead arranca directo en la fase de Arranque.

## API

| Método y ruta | Qué hace |
|---|---|
| `GET /api/proyectos/{id}/fases` | Estado completo (fases, criterios, actividades, historial) o `iniciado:false` con la fase sugerida |
| `POST …/fases/iniciar` | Inicia el motor en la fase que corresponde al estado del lead |
| `POST …/fases/criterio` `{fase, indice, ok}` | Marca/desmarca un criterio de la fase actual |
| `POST …/fases/avanzar` `{nota?, forzar?, resultado?, motivo?}` | Aprueba la puerta; en Negociación exige `resultado: GANADO\|PERDIDO` |
| `POST …/fases/retroceder` `{fase, motivo}` | Vuelve a una fase anterior |
| `GET …/prd/versiones` y `…/prd/versiones/{n}` | Versiones guardadas del PRD |

## Datos (`migrations/001_motor_de_fases.sql`, aditiva)

`ProyectoFase` (1 por Solución), `ProyectoFaseHistorial`, `FaseActividad`, `SolucionPrdVersion`. Prisma no las
conoce: solo las usa portalhub. Se aplican con `psql -f` (nunca `prisma db push --force-reset`).

## Pruebas

- `cargo test`: invariantes de la plantilla, mapa estado-del-lead → fase, criterios, vista.
- `tools/parity/fases.js` (en la VPS, contra el servicio real): ciclo completo preventa → venta → ejecución →
  cierre, ganado/perdido desde el lead y desde la puerta, permisos, PRD, sprints; limpia todo lo que crea.

## Pendiente (siguientes rebanadas)

Bandeja de aprobaciones y cartera de proyectos, dos ambientes por proyecto (demo vs producción), costos de
preventa por sprint, contexto del lead para los agentes, escalera de demo, plantillas de migración/implementación.

## Portada del Motor (Oficina > Motor)

Una sola pantalla (`GET /api/motor/resumen`, `routes/central.rs`; vista `oficina/motor/MotorView.tsx` en el portal) que junta:

- **Cartera:** cada proyecto con motor de fases, su fase, el avance de su puerta y sus tareas (hechas, corriendo, fallidas, bloqueadas).
- **Esperan a una persona:** puertas abiertas; las que ya tienen todos los criterios se pueden aprobar desde ahí (las de resultado Ganado/Perdido se deciden en el proyecto).
- **Agentes y cola:** lo que corre (arrancó hace menos de 2 h), lo fallido o bloqueado, lo atascado (IN_PROGRESS sin ejecución reciente, o ejecución RUNNING vieja) y la cola del Harness (`/queues`, con timeout; si no contesta se avisa).
- **Uso de tokens** por proyecto, de `TaskExecution.artifacts.usage`.

Es de solo lectura salvo el botón Aprobar, que llama a `fases/avanzar`. Se actualiza sola cada 20 s.
Límite conocido: no sabe si los workers están vivos (corren como otro usuario y no emiten señal).
Prueba: `tools/parity/motor_resumen.js`.

## Todo lead nace con motor

- `POST /api/leads` (y la conversión desde Prospección) crean la Solución del lead y arrancan el motor (`fases::proyecto_para_lead`). La respuesta incluye `solucionId`. Guardar un lead también asegura su proyecto.
- La Solución se llama «empresa — solución asociada» (o «Por definir»). Vaciar «solución asociada» al editar ya no borra la Solución.
- Los leads anteriores se inician desde Oficina > Motor («Leads sin motor»): `POST /api/motor/leads/{id}/iniciar` y `POST /api/motor/leads/iniciar-todos` (este último deja fuera los que ya tienen resultado).
- Oficina > Motor tiene «Nuevo lead», que usa el mismo `POST /api/leads`.
