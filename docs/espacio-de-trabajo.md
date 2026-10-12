# Espacio de trabajo unificado (MASD-0024-0013)

El hub de la solución (`/solutions/pilots/[id]`) deja de ser trece pestañas con formatos distintos y pasa a ser un espacio donde cada pieza
(«artefacto») se edita con el mismo modelo. Código: `src/routes/artefactos.rs`, `migrations/003_artefactos.sql`, `tools/parity/hub.js` (sección J);
en el portal `solutions/pilots/[id]/{espacio.ts,ArbolArtefactos,CabeceraArtefacto,DockAgente,CronogramaGantt}` y `components/ArchitectureCanvas.tsx`.

## El modelo

Siete artefactos, con la clave con la que los conoce el servidor: `prd`, `disenoTecnico`, `planEjecucion`, `arquitectura` (lienzo), `cronograma`,
`diagramas` y `planTrabajo`. Su contenido sigue en las columnas de `Solucion`; lo común se guarda aparte:

| Qué | Dónde | Para qué |
|---|---|---|
| Estado (borrador / en revisión / aprobado) | dentro del JSON (prd, diseño, plan de ejecución) o en `ArtefactoEstado` (el resto) | Aprobar con quién y cuándo; editar algo aprobado lo devuelve a revisión |
| Versiones | `SolucionPrdVersion` (PRD) y `ArtefactoVersion` (los demás, 50 por artefacto) | La versión anterior de cada edición guardada, con «traer al editor» |
| Comentarios | `ArtefactoComentario` | Conversación por artefacto, con «resolver» |
| Propuestas | `ArtefactoPropuesta` | Lo que el motor o una persona quiere cambiar; no se aplica hasta aceptarlo |

Mover un componente por el lienzo **no** es un cambio de contenido (no crea versión ni reabre lo aprobado); renombrarlo sí.

## API

- `GET /api/soluciones/{id}/artefactos` — estado, aprobación, versiones, comentarios abiertos y propuestas pendientes de las siete piezas (alimenta el árbol).
- `PUT /api/soluciones/{id}/artefactos/{clave}/estado` — `{ estado }`; aprobar es solo de administradores. La línea base se crea sola cuando PRD y diseño quedan aprobados.
- `GET …/artefactos/{clave}/versiones` y `…/versiones/{n}`; `GET|POST …/artefactos/{clave}/comentarios`; `PUT /api/comentarios-artefacto/{id}` `{ resuelto }`.
- `GET|POST /api/soluciones/{id}/propuestas` y `POST /api/propuestas-artefacto/{id}/decidir` `{ accion: aceptar|rechazar, nota }`.

### Propuestas

`ops` es una lista de operaciones, validadas al crearla y otra vez al aceptarla contra el contenido de ese momento:

- `arquitectura`: `{ op: "agregar", ruta: "nodes"|"connections", valor }` — el componente sin posición cae debajo del más bajo; una conexión a un componente inexistente se rechaza.
- `disenoTecnico`: `{ op: "agregar", ruta: "entidades"|"stack"|"integraciones"|"decisiones", valor }`.
- `cronograma`: `{ op: "desplazar", dias, desdeId? }` — la fase de origen alarga su fin y las que dependen de ella (en cadena) se corren completas; sin origen, se corren las fases no completadas.

Aceptar sobre un artefacto aprobado exige administrador; el contenido se reescribe con comparación y reemplazo (sin pisar una edición que llegó en medio), deja la versión anterior y
reabre lo aprobado. **El motor ya las crea solo:** cuando una tarea declara «CAMBIO DE DISEÑO:» en su resumen, o el revisor marca «DESVÍA DEL DISEÑO:», queda una propuesta de
decisión técnica en el diseño (`proponer_decision_de_diseno`), sin duplicar por tarea.

## La pantalla

- **Árbol** a la izquierda (reemplaza las pestañas en pantallas anchas; en angostas siguen las pestañas): piezas agrupadas por etapa, con punto de estado, contador de propuestas y de comentarios, y marca de «sin guardar».
- **Cabecera común** de toda pieza: estado, comentarios, historial, pedir revisión, aprobar/reabrir y guardar. Igual para un PRD, un lienzo o un cronograma.
- **Panel del agente** bajo el contenido: propuestas pendientes con lo que cambiarían (+ componente, + decisión, correr N días) y sus botones; no deja aceptar mientras la pieza tenga cambios sin guardar. Las últimas tareas del motor aparecen debajo.
- **Lienzo** (`ArchitectureCanvas`): además de componentes, notas adhesivas y grupos (mover el grupo mueve lo de adentro); deshacer/rehacer (Ctrl+Z / Ctrl+Y), duplicar (Ctrl+D), Supr, cuadrícula, propiedades siempre visibles (nombre, tipo, descripción, tamaño, conexiones), copiar como Mermaid, exportar JSON.
  Lo que propone el motor se dibuja punteado en ámbar sobre el propio lienzo. Notas y grupos no entran al contexto de los agentes.
- **Cronograma** (`CronogramaGantt`, vista por defecto; quedan Lista y Línea de tiempo): barras que se arrastran (mover, cambiar inicio o fin), dependencias dibujadas (rojas si chocan), mover dependientes en cadena, hitos, línea base con desvío, zoom día/semana/mes, hoy, progreso desde la tarea ligada. Lo propuesto por el motor se dibuja punteado.

## Límites (esta primera versión)

- El PRD, el diseño y el plan de ejecución siguen editándose con sus editores de siempre dentro del centro: la cabecera, el árbol y el panel son comunes, el editor de documentos por bloques (`/` para insertar tablas y diagramas dentro del texto) **no está hecho**.
- Las propuestas solo existen para arquitectura, diseño y cronograma; el PRD y el plan de ejecución todavía no las admiten.
- No hay un cuadro «pídele algo a este artefacto»: el panel del agente muestra propuestas y actividad, pero no recibe instrucciones.
- El lienzo no tiene selección múltiple ni alineación de varios elementos.

## Dentro de Oficina › Proyectos (MASD-0024-0014)

El hub ya no exige salir de Proyectos. `HubSolucion.tsx` (el antiguo `page.tsx`, que ahora es un envoltorio) acepta `embebido`: sin barra lateral ni título
de página, con la pestaña controlada desde fuera (`tabExterna`/`onTabChange`) y avisos hacia arriba (`onMeta`, `onSucio`). `ProyectosView` muestra una barra
de artefactos bajo las fases, monta el hub al abrir el primero y lo deja oculto (no desmontado) al volver al chat. El panel lateral tiene «Propuestas».
Enlaces: `/oficina?view=proyectos&p=<id>&art=<prd|diseno|plan-ejec|arquitectura|…>`; `/solutions/pilots/<id>[?seccion=x]` dentro de Oficina se traduce a eso.
El hub clásico sigue en `/solutions/pilots/<id>`.

## Notas por fase dentro del proyecto (adiós al Lead Hub)

Las notas y los archivos de cada fase se guardan en el proyecto (`ProyectoNota`, `ProyectoNotaArchivo`, migración 008; API `/api/proyectos/{id}/notas` y `archivos-nota`). Las vistas
`ProyectoNotaLead` y `ProyectoNotaArchivoLead` les dan a los lectores que antes leían `LeadHub` (IA, contexto de la propuesta, entregables) la misma forma por lead y etapa.
La verificación `nota_lead` mira las notas de la fase en el proyecto y sirve en cualquier fase. El diagrama del lead pasó a ser la arquitectura del proyecto (si estaba vacía).
Las tablas `LeadHub` y `LeadHubFile` quedaron vacías (respaldo en /root/respaldos); el Lead Hub conserva interacciones y propuesta.
