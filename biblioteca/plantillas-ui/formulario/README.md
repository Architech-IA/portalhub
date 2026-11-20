# Formulario

**Reglas**
- Etiqueta visible sobre cada campo (el placeholder NO es etiqueta). Obligatorios marcados con «(obligatorio)», no solo con asterisco.
- Validar al salir del campo (`onBlur`), no en cada tecla. El error va bajo el campo, dice qué pasó y cómo arreglarlo, y se anuncia con `role="alert"`.
- Tipo de entrada correcto (`email`, `tel`, `number`) y `autocomplete`. Alto de campo ≥ 44 px.
- Al fallar el envío, el foco va al primer campo con error. Si hay varios, un resumen arriba con enlaces a cada uno.
- El botón de enviar se deshabilita mientras envía, muestra progreso y luego el resultado. Una sola acción primaria; «Cancelar» es secundaria.
- Si hay cambios sin guardar, confirmar antes de salir.
- Agrupa campos relacionados (`fieldset` y `legend`).
- Seguridad: valida SIEMPRE también en el servidor y comprueba la sesión y el rol antes de escribir.

```tsx
'use client'
import { useId, useState } from 'react'

export function Campo({ etiqueta, error, ...p }: React.InputHTMLAttributes<HTMLInputElement> & { etiqueta: string; error?: string }) {
  const id = useId()
  return (
    <div className="space-y-1">
      <label htmlFor={id} className="text-sm font-medium">{etiqueta}{p.required ? ' (obligatorio)' : ''}</label>
      <input id={id} aria-invalid={!!error} aria-describedby={error ? `${id}-e` : undefined}
        className="h-11 w-full rounded-lg border px-3" style={{ borderColor: error ? 'var(--error)' : 'var(--linea)', background: 'var(--superficie)' }} {...p} />
      {error && <p id={`${id}-e`} role="alert" className="text-sm" style={{ color: 'var(--error)' }}>⚠ {error}</p>}
    </div>
  )
}

export function Enviar({ enviando, children }: { enviando: boolean; children: React.ReactNode }) {
  return (
    <button type="submit" disabled={enviando} className="h-11 rounded-lg px-5 font-medium disabled:opacity-60"
      style={{ background: 'var(--acento)', color: 'var(--acentoTexto)' }}>
      {enviando ? 'Guardando…' : children}
    </button>
  )
}
```
