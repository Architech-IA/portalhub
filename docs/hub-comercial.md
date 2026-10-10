# Hub de la solución para proyectos comerciales (MASD-0024-0012)

Revisión completa del hub (`/solutions/pilots/[id]`) y de su servidor, con las fallas de robustez y los huecos para proyectos
comerciales corregidos. Código: `src/routes/solucion_hub.rs` (nuevo), `gestion.rs`, `soluciones_ia.rs`, `backlog.rs`,
`motor/{ejecutor,diseno}.rs`; migración `migrations/002_hub_comercial.sql`; prueba `tools/parity/hub.js`.

## Qué fallaba y cómo quedó

| Falla | Ahora |
|---|---|
| «Guardar» enviaba los 8 documentos juntos y pisaba lo que otro (persona, sesión de IA, motor) había cambiado | El hub envía **solo lo que cambió**, cada documento con su `base` (cómo estaba al abrirlo). Si el servidor lo cambió, responde **409** con el contenido vigente y el hub pregunta: cargar lo vigente, guardar encima o seguir editando |
| El PUT reiniciaba `estado`, `valorEstimado`, `descripcion` y `leadId` si no venían | Solo se escribe lo que viene en el pedido |
| Sin historial de ediciones del PRD | Cada edición guardada deja la versión anterior en `SolucionPrdVersion` (últimas 50); botón **Historial** en el PRD |
| Aprobar un documento era un desplegable cualquiera | Solo administradores aprueban (403 si no); queda **quién y cuándo** (`aprobacion`); editar un documento aprobado lo devuelve a *En revisión* |
| Backlog desde el PRD: una tarea por llamada desde el navegador, el vínculo se guardaba solo al pulsar Guardar → duplicados | `POST /api/soluciones/{id}/backlog-desde-prd`: una **transacción** (bloqueo de la fila) que crea las tareas y guarda el vínculo; dos pedidos a la vez crean las tareas una sola vez |
| «Agregar todas al backlog» (cronograma) mandaba `solutionId` y las tareas nacían sin Solución | Corregido (`solucionId`) y el cronograma se guarda solo al terminar |
| Requisitos IMPLEMENTADO/VERIFICADO manuales | El requisito sigue a su tarea: hecha → IMPLEMENTADO; verificada por el Motor → VERIFICADO; falla o vuelve a la cola → APROBADO. Guardar el PRD no deshace ese avance (fusión de tres vías) |
| Riesgos e hitos: un PUT por tecla sin revisar la respuesta | Espera de 600 ms por fila, un solo PUT, error visible y envío pendiente al salir |
| El hub pedía **todo** el backlog y filtraba en el navegador | `GET /api/backlog?ligero=1&solucionId=…&conRequisito=1` |
| No se podía guardar una solución sin lead; un lead solo admitía una Solución | El lead es opcional; **solución adicional** (`parentId`) para fase 2 / mantenimiento del mismo cliente; si el lead ya tiene otra solución, 409 claro |
| Cualquier usuario podía eliminar una solución, sin aviso de lo que queda huérfano | Solo administradores; con backlog o sprints pide confirmar (`?forzar=1`) |
| Dos mapas de fases (el stepper del hub y el motor) | Una sola línea de vida: la tarjeta **Fase del proyecto** muestra la del motor; las otras tarjetas pasan a ser *Documentos* (sin los «próximamente») |
| HTML del editor enriquecido sin limpiar | `sanitizar.ts` (lista blanca de etiquetas, estilos y enlaces) al pintar |
| Opciones «adelanto visual» en el panel de IA | Se muestran solo las conectadas |

## Entregables y contenido de la IA

- **PRD con IA**: dos llamadas en paralelo (borrador sin requisitos + requisitos), cada una con un reintento si el JSON es inválido
  (antes un JSON inválido perdía todo). El contexto incluye ahora las **propuestas comerciales** (la aceptada es el alcance vendido),
  hasta 12.000 caracteres de notas de preventa (antes 6.000) y el diseño técnico si ya existe. Entre 12 y 25 requisitos (antes 8–12), cada uno con
  `fuente`: `propuesta`, `preventa` o `inferido` (el hub muestra «Inferido por la IA»). Medido: 22 requisitos en 82 s.
- **Arquitectura con IA**: hasta 24 componentes (antes 16).
- **Diagramas Mermaid** (nueva pestaña): modelo de datos (ER), secuencia, contexto/contenedores (C4) y flujo; se generan con IA desde el PRD y el diseño,
  se editan y se dibujan en el hub (Mermaid se carga bajo demanda desde jsDelivr). Los agentes del Motor los reciben como referencia
  (`motor/diseno.rs::bloque_diagramas`).
- **Revisión** (nueva pestaña): `GET /api/soluciones/{id}/coherencia` revisa sin IA ~20 reglas entre PRD, diseño, plan, cronograma, hitos, dinero y
  propuesta (por ejemplo: requisitos sin criterio, PRD aprobado sin tareas, calendario de pagos que no cuadra con el valor, cronograma que termina
  después del último hito, hitos vencidos, riesgos altos sin mitigación). `POST …/cruce-propuesta` compara con IA lo prometido en la propuesta con el PRD.
- **Paquete de entrega** (pestaña Entrega): un HTML con PRD, diseño, arquitectura, diagramas, plan de ejecución, cronograma, hitos con aceptación,
  cambios aprobados, riesgos y **acta de aceptación**; se descarga o se imprime a PDF.

## Dinero, cambios y aceptación (proyectos comerciales)

- **Hitos** (migración): `monto`, `estadoPago` (PENDIENTE/FACTURADO/PAGADO, solo administradores), `sprintId` (avance real del sprint) y aceptación del cliente
  (`aceptadoPor/En/Nota`, solo administradores). La pestaña Cumplimiento resume valor, calendario de pagos (avisa si no cuadra), facturado/pagado,
  tareas y tokens usados (con costo estimado si se indica el precio por millón).
- **Control de cambios** (tabla `SolicitudCambio`, pestaña Cambios): se describe el cambio, su costo y su plazo; un administrador lo aprueba o rechaza
  (queda quién y cuándo); aprobarlo suma el costo al valor del proyecto y rechazarlo lo revierte.
- **Fotografías** (`SolucionSnapshot`): la **línea base** se crea sola cuando PRD y diseño quedan aprobados; el **as-built** lo congela un administrador al entregar.
- **Seguimiento para el cliente**: `POST/DELETE /api/soluciones/{id}/enlace-cliente` (administradores) y página pública `/cliente/{token}` (sin sesión).
  Solo muestra fase, avance, hitos con aceptación y cambios aprobados; **no** dinero, PRD, riesgos ni código (la prueba lo verifica).
- **Cronograma**: dependencia entre fases, estado de la tarea ligada, aviso de fases vencidas y de choques entre fases dependientes.

## Qué NO cubre (límites conocidos)

- No hay horas de personas ni margen real: solo calendario de pagos, valor y tokens de IA (el precio por millón se escribe a mano en la pantalla).
- El cliente solo **ve** (no acepta desde el enlace): la aceptación la registra un administrador.
- Los diagramas no se comparan automáticamente con el código; el modelo de datos sigue comparándose con el esquema de Prisma como antes (`motor/diseno.rs`).
- Mermaid se descarga del CDN: sin internet el hub muestra el código y no el dibujo.
- La pantalla nueva no se probó en un navegador real (solo tipos, API y SSR); la prueba de integración cubre el servidor.
- `schema.prisma` del portal solo lleva las columnas nuevas; las tablas nuevas y las llaves foráneas las conoce solo portalhub (como las del motor de fases).
  **No** correr `prisma db push` sin revisar el diff.

## Probar

```bash
cd /root/portal-architechia && set -a && . ./.env && set +a
node /root/repos/portalhub/tools/parity/hub.js          # servidor: 60 comprobaciones, limpia lo que crea
node /root/repos/portalhub/tools/parity/hub.js --ia     # además PRD en dos llamadas y diagrama Mermaid con el modelo real
```
