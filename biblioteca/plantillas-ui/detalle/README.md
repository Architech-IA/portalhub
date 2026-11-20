# Vista de detalle

**Estructura:** miga de pan o botón «Volver» → cabecera (nombre, estado como punto + texto, acción primaria y menú de acciones) → datos agrupados en secciones → actividad reciente.

**Reglas**
- La cabecera responde «¿qué es esto y en qué estado está?» en una mirada. Una acción primaria; las demás, en un menú.
- Datos en parejas etiqueta/valor, agrupados por tema, con la etiqueta en tono atenuado. Valores vacíos se muestran como «—», no se ocultan.
- Edición en el sitio o en un panel lateral; nunca perder datos al cancelar (confirmar si hay cambios).
- Las acciones destructivas van aparte, con confirmación y el nombre del registro en el mensaje.
- La actividad lista quién hizo qué y cuándo, la más reciente arriba.
- La página tiene URL propia (se puede compartir) y «atrás» vuelve al listado con sus filtros.
- 375 px: una columna; las acciones pasan a una barra inferior.
- Seguridad: la ruta comprueba que la persona puede ver ESE registro, no solo que está autenticada. Datos de clientes o pagos nunca se exponen en vistas públicas.

```tsx
export function Datos({ titulo, filas }: { titulo: string; filas: [string, React.ReactNode][] }) {
  return (
    <section aria-labelledby={`t-${titulo}`} className="rounded-xl border p-4" style={{ borderColor: 'var(--linea)', background: 'var(--superficie)' }}>
      <h2 id={`t-${titulo}`} className="mb-3 text-sm font-semibold">{titulo}</h2>
      <dl className="grid grid-cols-[140px_1fr] gap-x-4 gap-y-2 text-sm">
        {filas.map(([k, v]) => (
          <div key={k} className="contents">
            <dt style={{ color: 'var(--textoAtenuado)' }}>{k}</dt>
            <dd>{v ?? '—'}</dd>
          </div>
        ))}
      </dl>
    </section>
  )
}
```
