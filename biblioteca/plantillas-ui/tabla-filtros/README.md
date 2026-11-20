# Tabla con filtros

**Estructura:** barra superior (búsqueda + filtros + acción primaria) → tabla → paginación o «cargar más».

**Reglas**
- Encabezado fijo; la primera columna identifica la fila; números alineados a la derecha con cifras tabulares.
- Ordenar: el encabezado es un botón (`aria-sort`) y muestra la dirección con icono y texto accesible.
- Búsqueda con debounce de 250 ms; los filtros activos se ven como píldoras que se pueden quitar.
- Estado por fila: punto + texto («● Disponible»), nunca solo color.
- Vacío: mensaje con la causa («Ningún lote coincide con estos filtros») y la acción («Limpiar filtros»).
- Cargando: esqueleto de filas con la misma altura (sin saltos). Error: mensaje con reintento.
- 375 px: la tabla va dentro de un contenedor con `overflow-x-auto` o se convierte en tarjetas apiladas.
- Acciones destructivas por fila piden confirmación y ofrecen deshacer.

```tsx
export function Tabla<T extends { id: string }>({ filas, columnas, vacio }: {
  filas: T[]; columnas: { clave: keyof T & string; titulo: string; derecha?: boolean }[]; vacio: string
}) {
  if (filas.length === 0) return <p className="py-10 text-center text-sm" role="status">{vacio}</p>
  return (
    <div className="overflow-x-auto rounded-xl border" style={{ borderColor: 'var(--linea)' }}>
      <table className="w-full text-sm">
        <thead className="sticky top-0" style={{ background: 'var(--superficieElevada)' }}>
          <tr>{columnas.map(c => (
            <th key={c.clave} scope="col" className={`px-3 py-2 text-left font-medium ${c.derecha ? 'text-right' : ''}`}>{c.titulo}</th>
          ))}</tr>
        </thead>
        <tbody>
          {filas.map(f => (
            <tr key={f.id} className="border-t" style={{ borderColor: 'var(--linea)' }}>
              {columnas.map(c => (
                <td key={c.clave} className={`px-3 py-3 ${c.derecha ? 'text-right tabular-nums' : ''}`}>{String(f[c.clave] ?? '')}</td>
              ))}
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  )
}
```
