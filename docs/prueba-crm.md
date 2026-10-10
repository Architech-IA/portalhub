# Lecciones aprendidas — CRM a la medida (prueba del Motor, 10/10/2026)

Cliente ficticio «Distribuidora Andina SAS». Recorrido completo por el motor de fases: de un lead nuevo a un CRM entregado, con el código construido por los agentes del Motor y cada decisión humana registrada.

## Números reales
| | |
|---|---|
| Fases recorridas | 12 de 12 (6 de preventa, 6 de ejecución) |
| Tareas de código por agentes | 21 ejecuciones en 3 sprints (MVP, endurecimiento, entrega) |
| Actividades de fase por agentes | 12 ejecuciones |
| Tiempo de agentes | 97 minutos |
| Tokens | 6,18 millones (3,9 M solo el sprint de endurecimiento) |
| Código final en `main` | 34 archivos TypeScript, 4.719 líneas, 50 archivos cambiados |
| Pruebas automáticas | 32 unitarias + 2 rondas de aceptación en navegador (48 comprobaciones) |
| Reloj total | unas 2 h 40 min, incluidas las esperas y los arreglos al Motor |

## Lo que funcionó
- El Motor construyó los 8 requisitos del MVP solo (21 min, `tsc` limpio, revisor 8/8) y el CRM resultante funciona en un navegador real.
- La conversión de venta conservó la misma Solución, el repositorio y la demo; el PRD de preventa quedó como versión 1.
- La revisión humana del PR encontró lo que ninguna comprobación automática vio.
- Dividir tareas grandes y reintentar con instrucciones precisas resolvió todos los fallos de tareas.

## Lo que se rompió y se arregló durante la prueba
1. **Los agentes no recibían los datos del lead:** la ficha del cliente llegaba sin el nombre de la empresa. Ahora reciben lead, notas por etapa, propuestas y fase.
2. **El Motor no podía subir la rama ni abrir el PR** de un repositorio independiente (sin credenciales). Corregido; los PR 2 y 3 los abrió el propio Motor.
3. **`prisma migrate deploy` bajaba Prisma 7** y rechazaba el esquema de Prisma 5. Ahora usa la versión que fija el proyecto.
4. **Los despliegues publicaban el puerto de la aplicación en todas las interfaces** (HTTP plano, saltándose HTTPS). Ahora solo en `127.0.0.1`.
5. **Una vulnerabilidad en el código de los agentes:** la acción de alta de jefatura inicial se podía invocar directamente y crear administradores. La encontró la revisión del PR, se corrigió con una tarea y se comprobó en producción con una prueba de ataque.

## Lo que sigue pendiente (ya en el backlog)
- Las tareas están limitadas a 20 rondas de herramientas: tareas que tocan muchos archivos o requieren explorar mucho código **fallan y pierden todo el trabajo** (el worktree se descarta). Tres tareas fallaron por esto y costaron 1,4 millones de tokens.
- Las actividades de fase con agentes de código **no ven el repositorio**: el agente de «revisar la calidad del MVP» concluyó que no había MVP y gastó 188 mil tokens.
- Lo que escriben los agentes de documentación de preventa solo queda en el resultado de la tarea (no en el Lead Hub ni en los documentos del hub).
- Aprobar un sprint solo lo cierra: no mergea ni despliega. La base del repositorio (package.json, lockfile, esquema) la arma una persona.
- La migración usa la imagen ya construida: hay que desplegar antes de migrar.
- Demo y producción comparten el mismo despliegue; el monitoreo del contenedor del proyecto no existe.
- En el CRM entregado: sin límite de intentos de inicio de sesión, sin recuperación de clave, el alta inicial queda abierta hasta crear la primera jefatura.

## Recomendaciones para el siguiente proyecto
1. Tareas de código de **máximo 4 a 6 archivos** y con la instrucción de escribir primero y verificar después.
2. Armar la base del repositorio (dependencias, esquema, migración) y el diseño técnico **antes** del primer sprint.
3. Revisión humana de cada PR con foco en seguridad; las pruebas automáticas y el revisor semántico no la reemplazan.
4. Crear la jefatura inicial del cliente inmediatamente después de desplegar.
5. Estimar con el costo real: este CRM costó 97 minutos de agentes y 6 M de tokens, más el tiempo humano de arquitectura, revisión y gestión.
