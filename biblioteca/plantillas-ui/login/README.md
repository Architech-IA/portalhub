# Inicio de sesión

**Estructura:** logo o nombre del producto → título «Iniciar sesión» → correo → contraseña (con mostrar/ocultar) → botón primario → enlace de ayuda.

**Reglas de interfaz**
- Una sola columna centrada, ancho máximo de 400 px. Campos de 44 px con `autocomplete="username"` y `autocomplete="current-password"` (los gestores de contraseñas deben funcionar).
- Botón para mostrar u ocultar la contraseña, con `aria-label` y `aria-pressed`.
- Error genérico y útil: «Correo o contraseña incorrectos» (no reveles cuál de los dos falló). Se anuncia con `role="alert"`.
- Mientras valida: botón deshabilitado y con texto «Entrando…».
- Enter envía el formulario. Foco inicial en el correo.
- Si hay proveedor externo (Google, Microsoft), el botón lleva su texto completo y no sustituye al formulario.

**Reglas de seguridad (obligatorias, en el servidor)**
- Límite de intentos por cuenta y por IP (por ejemplo 5 por 15 minutos) y bloqueo temporal.
- Contraseñas con hash (argon2 o bcrypt); jamás en texto plano ni en logs.
- Cookie de sesión `HttpOnly`, `Secure`, `SameSite=Lax`; rotar la sesión al iniciar.
- La ruta de login es pública, pero TODA ruta que modifica datos comprueba sesión y rol.
- Nunca incluyas usuarios o claves de ejemplo en el código de producción.

```tsx
'use client'
import { useState } from 'react'

export default function Login() {
  const [ver, setVer] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const [enviando, setEnviando] = useState(false)
  async function enviar(e: React.FormEvent<HTMLFormElement>) {
    e.preventDefault(); setEnviando(true); setError(null)
    const f = new FormData(e.currentTarget)
    const r = await fetch('/api/login', { method: 'POST', body: JSON.stringify(Object.fromEntries(f)), headers: { 'Content-Type': 'application/json' } })
    if (!r.ok) setError('Correo o contraseña incorrectos'); else location.href = '/'
    setEnviando(false)
  }
  return (
    <main className="mx-auto flex min-h-dvh max-w-[400px] flex-col justify-center gap-4 px-4">
      <h1 className="text-2xl font-semibold">Iniciar sesión</h1>
      <form onSubmit={enviar} className="space-y-4">
        <label className="block text-sm font-medium">Correo
          <input name="email" type="email" required autoComplete="username" autoFocus className="mt-1 h-11 w-full rounded-lg border px-3" /></label>
        <label className="block text-sm font-medium">Contraseña
          <span className="mt-1 flex gap-2"><input name="password" type={ver ? 'text' : 'password'} required autoComplete="current-password" className="h-11 w-full rounded-lg border px-3" />
            <button type="button" aria-pressed={ver} aria-label="Mostrar contraseña" onClick={() => setVer(v => !v)} className="h-11 px-3">{ver ? 'Ocultar' : 'Ver'}</button></span></label>
        {error && <p role="alert" className="text-sm" style={{ color: 'var(--error)' }}>⚠ {error}</p>}
        <button disabled={enviando} className="h-11 w-full rounded-lg font-medium" style={{ background: 'var(--acento)', color: 'var(--acentoTexto)' }}>{enviando ? 'Entrando…' : 'Entrar'}</button>
      </form>
    </main>
  )
}
```
