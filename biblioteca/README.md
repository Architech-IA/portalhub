# Biblioteca de conocimiento y diseño

Material que usan los agentes del portal y las personas del equipo: skills, sistemas de diseño y plantillas de paneles.
Vive en este repositorio (portalhub) y se cambia por pull request, como el código.

## Estructura

```
biblioteca/
  skills/<nombre>/            FICHA.json, RESUMEN.md (lo que se inyecta al agente), referencias/ (detalle para personas)
  sistemas-diseno/<nombre>/   FICHA.json, README.md (guía), sistema.json (tokens)
  plantillas-ui/<nombre>/     FICHA.json, README.md (cuándo usarla y el código de partida)
  sincronizar.sh / .ps1       copia al ~/.claude/skills de una persona las skills marcadas para Claude Code
```

## FICHA.json

| Campo | Para qué |
|---|---|
| `descripcion` | Una línea: qué es |
| `cuandoUsar` | Cuándo conviene cargarlo |
| `palabrasClave` | Palabras que, si aparecen en la tarea, lo hacen relevante (cada una suma 2 puntos; se carga con 2 o más) |
| `roles` | Áreas de agente donde aplica (suma 1 punto si ya hay una palabra clave) |
| `version`, `dueno` | Control de cambios |
| `licencia`, `origen` | De dónde viene y bajo qué licencia (obligatorio si es de terceros) |
| `sincronizarClaudeCode` | `true` solo si la carpeta trae un `SKILL.md` listo para `~/.claude/skills` |

## Cómo los usa el motor

Solo en tareas de **interfaz** (la tarea o su descripción nombran pantallas, componentes, paneles, estilos, formularios,
login, dashboard...). El motor añade al contexto: el sistema de diseño elegido para el proyecto (manda sobre cualquier otra
referencia), hasta una plantilla de panel y hasta dos skills, recortados a un presupuesto de caracteres. Las tareas que no
tocan interfaz no reciben nada de la biblioteca.

## Reglas para agregar contenido

1. Si viene de un tercero, anota `licencia` y `origen` en la ficha y deja un `NOTICE.md` en la carpeta.
2. El `RESUMEN.md` / `README.md` principal no pasa de ~3.500 caracteres: es lo que cabe en el contexto de una tarea.
3. El detalle largo va en `referencias/` (se ve desde el portal, no se inyecta).
4. Una skill pensada para Claude Code lleva además su `SKILL.md` original y `sincronizarClaudeCode: true`.
5. Medir antes de confiar: los modelos de OpenCode pueden seguir estas instrucciones peor que Claude. Cualquier skill nueva
   se prueba con una tarea de interfaz de ejemplo antes de darla por buena.
