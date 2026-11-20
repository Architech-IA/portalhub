# Panel de indicadores (dashboard)

**Estructura, de arriba abajo:** título y filtro de periodo → fila de 3 a 4 KPIs → un gráfico de tendencia → lista «Requiere atención».

**Reglas**
- Lo resumido va ANTES que el detalle. Cada KPI: etiqueta, valor grande, variación y una mini línea; el estado lleva icono o texto, no solo color.
- Un KPI solo se muestra si alguien decide algo con él. Máximo 4.
- Gráfico: ejes con unidades, leyenda y tooltip; no uses 5 colores si 2 bastan.
- Estados: cargando (esqueleto), vacío (mensaje y acción) y error (con reintento).
- 375 px: KPIs en una columna; el gráfico hace scroll horizontal dentro de su contenedor.

```tsx
type Kpi = { etiqueta: string; valor: string; variacion?: string; estado?: 'ok' | 'aviso' | 'error' }
const ICONO = { ok: '▲', aviso: '●', error: '▼' }

export function Kpis({ items }: { items: Kpi[] }) {
  return (
    <ul className="grid gap-3 sm:grid-cols-2 lg:grid-cols-4">
      {items.map(k => (
        <li key={k.etiqueta} className="rounded-xl border p-4"
            style={{ background: 'var(--superficie)', borderColor: 'var(--linea)' }}>
          <p className="text-xs" style={{ color: 'var(--textoAtenuado)' }}>{k.etiqueta}</p>
          <p className="mt-1 text-3xl font-semibold tabular-nums">{k.valor}</p>
          {k.variacion && (
            <p className="mt-1 text-xs" style={{ color: `var(--${k.estado === 'error' ? 'error' : k.estado === 'aviso' ? 'aviso' : 'exito'})` }}>
              {ICONO[k.estado ?? 'ok']} {k.variacion}
            </p>
          )}
        </li>
      ))}
    </ul>
  )
}
```
Define las variables CSS (`--superficie`, `--linea`, `--exito`...) desde `sistema.json` del proyecto en `:root`.
