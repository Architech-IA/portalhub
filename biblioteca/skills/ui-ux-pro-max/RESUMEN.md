# Reglas de UX por prioridad

Revisa en este orden; lo de arriba pesa más que lo de abajo.

1. **Accesibilidad (crítico).** Contraste de texto ≥ 4,5:1 (texto grande 3:1). Foco visible en todo lo interactivo. Texto alternativo en imágenes con significado. `aria-label` en botones solo con icono. Orden de tabulación igual al visual. Los encabezados siguen h1, h2, h3 sin saltos. Nada se comunica solo con color. Respeta `prefers-reduced-motion`.
2. **Táctil e interacción (crítico).** Objetivos ≥ 44×44 px con ≥ 8 px entre ellos. Lo primario se hace con clic o toque, nunca solo con hover. Los botones se deshabilitan y muestran progreso durante una operación asíncrona. Los errores aparecen junto al problema. Cursor de mano en lo clicable.
3. **Rendimiento.** Imágenes WebP/AVIF con ancho y alto declarados (evita saltos de diseño); carga diferida de lo que no se ve; fuentes con `font-display: swap`; listas de más de 50 elementos virtualizadas; esqueletos en vez de spinners largos.
4. **Estilo.** Un estilo coherente por producto; iconos SVG de una sola familia, nunca emojis; elevaciones y sombras de una escala fija; un único CTA primario por pantalla; diseña claro y oscuro juntos.
5. **Layout y responsive.** Mobile primero; viewport sin desactivar el zoom; sin scroll horizontal; escala de espaciado de 4/8 px; ancho máximo de contenedor consistente en escritorio; escala de z-index definida; usa `min-h-dvh` en lugar de `100vh` en móvil.
6. **Tipografía y color.** Cuerpo 16 px, interlineado 1,5 a 1,75, 65 a 75 caracteres por línea; escala tipográfica fija; tokens semánticos (primario, error, superficie, texto) y no valores sueltos; en oscuro usa tonos desaturados, no colores invertidos; cifras tabulares en datos.
7. **Animación.** 150 a 300 ms para microinteracciones; solo `transform` y `opacity`; entrada con ease-out y salida más corta; una o dos animaciones clave por vista; nunca bloquees la interacción mientras anima.
8. **Formularios y retroalimentación.** Etiqueta visible por campo; marca los obligatorios; validación al salir del campo, no en cada tecla; error con causa y solución; estado de éxito breve; confirma antes de acciones destructivas y ofrece deshacer; vacíos con mensaje y acción; toasts de 3 a 5 s sin robar el foco.
9. **Navegación.** Comportamiento de «atrás» predecible; navegación inferior de máximo 5 elementos con icono y texto; la ubicación actual se ve; las pantallas clave tienen URL propia.
10. **Gráficos y datos.** Leyenda y tooltip; colores accesibles y distinguibles sin depender del tono; un gráfico no se dibuja con cinco colores si dos bastan; números comparables en una sola forma (tabla o gráfico, no ambos).

Detalle completo del original: referencias/SKILL.md.
