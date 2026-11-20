# Diseño de interfaces: criterio de calidad

Adaptado para agentes del portal. Aplícalo SOLO a lo que la tarea pide; no rediseñes lo que no se te pidió.

## Antes de escribir código
1. **Respeta lo que ya existe.** Si el proyecto tiene sistema de diseño, tokens o componentes, úsalos tal cual. Lo que pida la tarea manda sobre tu gusto.
2. **Define el modo de la pantalla.** Operar (dashboards, formularios, admin: escaneo rápido, consistencia), Leer (documentación, informes), Persuadir (landing, campañas) o Experimentar (portafolios). El modo decide cuánta expresión cabe.
3. **Refinar conserva; rediseñar reemplaza.** Si te piden un ajuste, no cambies identidad, textos ni comportamiento fuera del alcance.

## Reglas de calidad
- **Jerarquía clara:** una acción primaria por pantalla; el resto, secundario. Lo importante se ve primero por tamaño, peso y espacio, no solo por color.
- **Tipografía:** máximo dos familias; una escala fija (por ejemplo 12, 14, 16, 20, 28, 40); cuerpo ≥ 16 px; líneas de 60 a 75 caracteres; números en columnas con cifras tabulares.
- **Color:** define tokens (fondo, superficie, texto, texto atenuado, línea, acento, estados) y no uses valores sueltos. Contraste de texto ≥ 4,5:1. El color nunca es la única señal: añade texto, icono o patrón.
- **Espaciado:** una escala (4, 8, 12, 16, 24, 32, 48). Agrupa con espacio antes que con cajas. No pongas borde, sombra y relleno a todo.
- **Estados:** cada control define reposo, hover, foco visible, activo, deshabilitado y cargando. Una lista o tabla define también vacío, error y cargando.
- **Formularios:** etiqueta visible (no solo placeholder), error junto al campo con cómo arreglarlo, botón de envío que muestra progreso y resultado.
- **Responsive:** funciona a 375 px sin scroll horizontal; objetivos táctiles ≥ 44 px; las tablas anchas hacen scroll dentro de su contenedor.
- **Movimiento:** solo si explica algo (150 a 300 ms, `transform` y `opacity`); respeta `prefers-reduced-motion`.
- **Texto de interfaz:** nombra las cosas como las reconoce la persona, no como las nombra el sistema. Los controles dicen qué hacen ("Guardar cambios"). Los errores dicen qué pasó y qué hacer.

## Errores típicos que delatan un diseño genérico (evítalos)
- Degradado morado a azul sobre blanco como portada; emojis como iconos o viñetas; todo centrado; esquinas redondeadas y sombra idénticas en todas las tarjetas; tarjetas con barra de color lateral.
- Texto gris sobre fondo gris; placeholder como única etiqueta; iconos sin nombre accesible; foco invisible.
- Contenido de relleno («Lorem ipsum»): usa datos de ejemplo realistas y márcalos como ejemplo.
- Numerar secciones o pasos cuando no son una secuencia real.

## Antes de dar la tarea por hecha
- Revisa a 375 px y a 1280 px, en claro y en oscuro si el proyecto tiene ambos.
- Navega solo con teclado: el orden y el foco deben tener sentido.
- Confirma que no hay colores ni fuentes fuera de los tokens del proyecto.
- No dejes texto cortado, controles solapados ni scroll horizontal en la página.

Detalle por tema (para personas): referencias/typeset.md, layout.md, colorize.md, craft-floor.md, critique.md, polish.md, harden.md.
