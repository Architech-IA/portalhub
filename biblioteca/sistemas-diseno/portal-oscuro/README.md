# Sistema «portal-oscuro»

Oscuro, con tarjetas de cristal y un solo acento (índigo). Mira y se siente como el portal de ArchiTechIA.

## Reglas de uso
- **Fondo y capas:** fondo `#0b0f17`; las tarjetas son una capa clara muy tenue (`superficie`) con borde `linea`. No apiles más de dos niveles de capa.
- **Un solo acento:** el índigo es para la acción primaria y el elemento activo. Los estados usan sus propios colores (éxito, aviso, error, info) y siempre llevan texto o icono además del color.
- **Texto:** `texto` para lo principal, `textoSuave` para apoyo, `textoAtenuado` para metadatos. Contraste mínimo 4,5:1: no uses `textoAtenuado` para información necesaria.
- **Tipografía:** títulos en Space Grotesk, cuerpo en Inter 14 px, cifras y códigos en JetBrains Mono con cifras tabulares.
- **Cristal:** `backdrop-filter` solo en paneles flotantes (menús, paneles laterales). OJO: un ancestro con `backdrop-filter` o `transform` atrapa a los hijos `position: fixed`; los modales se renderizan con un portal en `document.body`.
- **Píldoras de estado:** punto de color + texto («● Aprobado»), nunca solo el color.
- **Densidad:** herramienta de trabajo: filas de 36 a 44 px, texto de 12 a 14 px, objetivos táctiles de 44 px en móvil.
- **Acceso por teclado:** foco visible con un anillo `acento` de 2 px y desplazamiento de 2 px.

## Cómo cargar las fuentes
Google Fonts: Space Grotesk (500, 600, 700), Inter (400, 500, 600) y JetBrains Mono (400, 500), con `display=swap` y una pila de respaldo.
