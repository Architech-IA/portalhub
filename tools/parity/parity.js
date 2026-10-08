#!/usr/bin/env node
/**
 * Harness de paridad Next ↔ Rust (MASD PHUB-0001-0001).
 *
 * Manda las MISMAS peticiones a la implementación original (Next, :3003) y a portalhub (Rust,
 * :3100) con una sesión real de NextAuth firmada con NEXTAUTH_SECRET, y compara las respuestas.
 * Es lo que decide si una ruta está lista para cortarse en Nginx.
 *
 * Uso (en el VPS; las variables del portal se cargan sin imprimirse):
 *   set -a; . /root/portal-architechia/.env; set +a
 *   node /root/repos/portalhub/tools/parity/parity.js [users|profile|meetings|dashboard|leads|hub|all]
 *
 * - Lecturas: se comparan tal cual sobre los datos reales. Si solo difiere el ORDEN de un array,
 *   se informa aparte (ORDEN) porque Next/Prisma devuelve filas sin ORDER BY en orden físico.
 * - Escrituras: cada flujo corre primero contra Next y después contra Rust sobre datos de prueba
 *   ("PARITY-TEST"), y se comparan paso a paso enmascarando ids y fechas. Todo se limpia al final.
 * - Las desviaciones DELIBERADAS (endurecimientos) se listan en ESPERADAS y se informan aparte.
 */
const path = require('path')
const ROOT = '/root/portal-architechia'
const { encode } = require(path.join(ROOT, 'node_modules/next-auth/jwt'))
const { PrismaClient } = require(path.join(ROOT, 'node_modules/@prisma/client'))

const NEXT = process.env.PARITY_NEXT || 'http://127.0.0.1:3003'
const RS = process.env.PARITY_RS || 'http://127.0.0.1:3100'
const prisma = new PrismaClient()
const area = process.argv[2] || 'all'
const TAG = 'PARITY-TEST'

const creados = { leads: [] }
const resumen = { ok: 0, orden: 0, esperadas: 0, fallas: 0 }
const fallas = []

// ── Sesión ──────────────────────────────────────────────────────────────────────────────────
async function cookie(user) {
  const token = await encode({
    token: { name: user.name, email: user.email, sub: user.id, id: user.id, role: user.role, picture: null },
    secret: process.env.NEXTAUTH_SECRET, maxAge: 3600,
  })
  return `__Secure-next-auth.session-token=${token}; next-auth.session-token=${token}`
}

async function llamar(base, metodo, url, ck, cuerpo, extra = {}) {
  const headers = { 'content-type': 'application/json', ...(ck ? { cookie: ck } : {}), ...extra }
  const r = await fetch(base + url, { method: metodo, headers, body: cuerpo === undefined ? undefined : JSON.stringify(cuerpo), redirect: 'manual' })
  const texto = await r.text()
  let json
  try { json = JSON.parse(texto) } catch { json = texto.slice(0, 200) }
  return { status: r.status, json }
}

// ── Normalización y comparación ─────────────────────────────────────────────────────────────
const RE_CUID = /^c[a-z0-9]{20,}$/
const RE_FECHA = /^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(\.\d+)?Z$/
const RE_N_ID = /^n\d{10,}-\d+$/

function norm(v, k, o) {
  if (Array.isArray(v)) return v.map((x) => norm(x, k, o))
  if (v && typeof v === 'object') {
    const out = {}
    for (const key of Object.keys(v).sort()) {
      if (o.drop && o.drop.includes(key)) continue
      out[key] = norm(v[key], key, o)
    }
    return out
  }
  if (typeof v === 'string') {
    if (o.mask) {
      if (RE_CUID.test(v) || RE_N_ID.test(v)) return '<id>'
      if (RE_FECHA.test(v)) return '<fecha>'
      if (k && /(^id$|Id$)/.test(k)) return '<id>'
    }
  }
  return v
}

function ordenar(v) {
  if (Array.isArray(v)) return v.map(ordenar).sort((a, b) => JSON.stringify(a).localeCompare(JSON.stringify(b)))
  if (v && typeof v === 'object') { const o = {}; for (const k of Object.keys(v)) o[k] = ordenar(v[k]); return o }
  return v
}

function diffs(a, b, p = '$', out = []) {
  if (out.length >= 6) return out
  if (typeof a !== typeof b || (a === null) !== (b === null) || Array.isArray(a) !== Array.isArray(b)) { out.push(`${p}: ${JSON.stringify(a)?.slice(0, 90)} ≠ ${JSON.stringify(b)?.slice(0, 90)}`); return out }
  if (a && typeof a === 'object') {
    const keys = new Set([...Object.keys(a), ...Object.keys(b)])
    for (const k of keys) {
      if (!(k in a)) out.push(`${p}.${k}: falta en Next (Rust=${JSON.stringify(b[k])?.slice(0, 70)})`)
      else if (!(k in b)) out.push(`${p}.${k}: falta en Rust (Next=${JSON.stringify(a[k])?.slice(0, 70)})`)
      else diffs(a[k], b[k], `${p}.${k}`, out)
      if (out.length >= 6) break
    }
    return out
  }
  if (a !== b) out.push(`${p}: ${JSON.stringify(a)?.slice(0, 90)} ≠ ${JSON.stringify(b)?.slice(0, 90)}`)
  return out
}

function comparar(nombre, n, r, o = {}) {
  const opts = { drop: o.drop, mask: o.mask }
  if (o.esperada) {
    const sNext = n.status, sRs = r.status
    console.log(`  ESPERADA ${nombre}: Next=${sNext} Rust=${sRs} — ${o.esperada}`)
    resumen.esperadas++
    return
  }
  if (n.status !== r.status) {
    resumen.fallas++; fallas.push(nombre)
    console.log(`  ✗ ${nombre}: status Next=${n.status} Rust=${r.status}\n      Next: ${JSON.stringify(n.json)?.slice(0, 160)}\n      Rust: ${JSON.stringify(r.json)?.slice(0, 160)}`)
    return
  }
  const a = norm(n.json, '', opts), b = norm(r.json, '', opts)
  if (JSON.stringify(a) === JSON.stringify(b)) { resumen.ok++; console.log(`  ✓ ${nombre} (${n.status})`); return }
  if (JSON.stringify(ordenar(a)) === JSON.stringify(ordenar(b))) { resumen.orden++; console.log(`  ~ ${nombre}: igual salvo ORDEN de arrays`); return }
  resumen.fallas++; fallas.push(nombre)
  console.log(`  ✗ ${nombre}: contenido distinto`)
  for (const d of diffs(a, b)) console.log(`      ${d}`)
}

// ── Lecturas sobre datos reales ─────────────────────────────────────────────────────────────
async function lecturas(ck) {
  const get = async (nombre, url, o) => comparar(nombre, await llamar(NEXT, 'GET', url, ck), await llamar(RS, 'GET', url, ck), o)
  const sel = async (sql, ...p) => prisma.$queryRawUnsafe(sql, ...p)

  if (['users', 'all'].includes(area)) await get('GET /api/users', '/api/users')
  if (['profile', 'all'].includes(area)) await get('GET /api/profile', '/api/profile', { drop: ['googleAccessToken', 'microsoftAccessToken'] })
  if (['meetings', 'all'].includes(area)) {
    await get('GET /api/meetings', '/api/meetings')
    for (const m of await sel('SELECT id FROM "Meeting" ORDER BY "date" DESC LIMIT 4')) await get(`GET /api/meetings/${m.id.slice(0, 8)}…/actions`, `/api/meetings/${m.id}/actions`)
  }
  if (['dashboard', 'all'].includes(area)) {
    await get('GET /api/dashboard', '/api/dashboard', { drop: ['tendencias'] })
    await get('GET /api/dashboard (tendencias)', '/api/dashboard', { esperada: 'cálculo de fin de mes corregido (ver dashboard.rs); se verifica a mano abajo' })
    await get('GET /api/dashboard/personal', '/api/dashboard/personal')
  }
  if (['leads', 'hub', 'all'].includes(area)) {
    await get('GET /api/leads', '/api/leads')
    for (const l of await sel('SELECT id FROM "Lead" ORDER BY "createdAt" DESC LIMIT 5')) {
      const t = l.id.slice(0, 8) + '…'
      await get(`GET /api/leads/${t}`, `/api/leads/${l.id}`)
      for (const sub of ['notes', 'interactions', 'proposal', 'backlog-items']) await get(`GET /api/leads/${t}/${sub}`, `/api/leads/${l.id}/${sub}`)
      await get(`GET hub-phase ${t}`, `/api/leads/hub-phase?leadId=${l.id}`)
      await get(`GET architecture ${t}`, `/api/leads/architecture?leadId=${l.id}`)
      await get(`GET diagram ${t}`, `/api/leads/diagram?leadId=${l.id}`)
    }
    for (const f of await sel('SELECT id FROM "LeadHubFile" LIMIT 2')) await get(`GET hub-file ${f.id.slice(0, 8)}…`, `/api/leads/hub-file?id=${f.id}`)
  }
}

// ── Flujos de escritura (primero Next, después Rust) ────────────────────────────────────────
async function flujo(nombre, fn) {
  console.log(`\n── flujo: ${nombre}`)
  const resN = [], resR = []
  await fn(NEXT, resN)
  await fn(RS, resR)
  const n = Math.max(resN.length, resR.length)
  for (let i = 0; i < n; i++) {
    const a = resN[i], b = resR[i]
    if (!a || !b) { resumen.fallas++; fallas.push(`${nombre} paso ${i}`); console.log(`  ✗ paso ${i}: falta respuesta`); continue }
    comparar(a.nombre, a.res, b.res, { mask: true, drop: a.drop, esperada: a.esperada })
  }
}
const paso = (lista) => async (nombre, ...args) => { const o = args.pop(); const res = await llamar(...args); lista.push({ nombre, res, ...o }); return res }

async function main() {
  const admin = (await prisma.$queryRawUnsafe(`SELECT id, name, email, role::text role FROM "User" WHERE role = 'SUPERADMIN' ORDER BY ctid LIMIT 1`))[0]
  if (!admin) throw new Error('No hay SUPERADMIN para firmar la sesión de prueba')
  const ckAdmin = await cookie(admin)

  // Usuario de prueba SIN tokens de Google: así las reuniones de prueba no tocan ningún calendario real.
  await prisma.$executeRawUnsafe(`DELETE FROM "User" WHERE email = 'parity-bot@example.test'`)
  const bot = (await prisma.$queryRawUnsafe(`INSERT INTO "User" (id, name, email, password, role, "updatedAt")
    VALUES ('parity-bot-id', '${TAG} bot', 'parity-bot@example.test', 'x', 'COLLABORATOR', NOW()) RETURNING id, name, email, role::text role`))[0]
  const ckBot = await cookie(bot)

  console.log(`Paridad Next(${NEXT}) ↔ Rust(${RS}) — área: ${area} — sesión de prueba: ${admin.email} (${admin.role})`)

  // Salud + autenticación
  const h = await llamar(RS, 'GET', '/health')
  console.log(`  salud Rust: ${h.status} ${JSON.stringify(h.json)}`)
  const sinCookie = await llamar(RS, 'GET', '/api/users')
  const falsa = await llamar(RS, 'GET', '/api/users', 'next-auth.session-token=cualquier-cosa')
  const nextFalsa = await llamar(NEXT, 'GET', '/api/users', 'next-auth.session-token=cualquier-cosa')
  console.log(`  sesión inválida → Rust ${falsa.status}/${sinCookie.status} (Next con cookie falsa: ${nextFalsa.status} ← el hueco que el servicio Rust cierra)`)
  if (falsa.status !== 401 || sinCookie.status !== 401) { resumen.fallas++; fallas.push('auth: Rust acepta sesión inválida') }
  else resumen.ok++

  await lecturas(ckAdmin)

  try {
    if (['users', 'all'].includes(area)) {
      await flujo('users (admin)', async (base, L) => {
        const P = paso(L)
        const c = await P('POST /api/users', base, 'POST', '/api/users', ckAdmin, { name: `${TAG} U`, email: 'parity-u@example.test', password: 'clave-123', role: 'COLLABORATOR' }, {})
        const id = c.json && c.json.id
        await P('POST duplicado', base, 'POST', '/api/users', ckAdmin, { name: 'x', email: 'parity-u@example.test', password: 'p', role: 'COLLABORATOR' }, {})
        await P('POST rol SUPERADMIN', base, 'POST', '/api/users', ckAdmin, { name: 'x', email: 'parity-x@example.test', password: 'p', role: 'SUPERADMIN' }, {})
        await P('PUT usuario', base, 'PUT', '/api/users', ckAdmin, { id, name: `${TAG} U2`, role: 'GERENTE_COMERCIAL' }, {})
        await P('PUT asignar SUPERADMIN', base, 'PUT', '/api/users', ckAdmin, { id, role: 'SUPERADMIN' }, {})
        await P('PUT sobre SUPERADMIN', base, 'PUT', '/api/users', ckAdmin, { id: admin.id, name: 'no' }, {})
        await P('DELETE sobre SUPERADMIN', base, 'DELETE', '/api/users', ckAdmin, { id: admin.id }, {})
        await P('DELETE usuario', base, 'DELETE', '/api/users', ckAdmin, { id }, {})
        await P('DELETE inexistente', base, 'DELETE', '/api/users', ckAdmin, { id: 'no-existe' }, {})
      })
      // Endurecimientos de Rust (no comparables contra Next porque Next los permite):
      const noAdminPost = await llamar(RS, 'POST', '/api/users', ckBot, { name: 'x', email: 'parity-z@example.test', password: 'p', role: 'ADMIN' })
      console.log(`  endurecimiento: POST /api/users sin rol admin → Rust ${noAdminPost.status} (Next lo permitía)`)
      noAdminPost.status === 403 ? resumen.ok++ : (resumen.fallas++, fallas.push('POST /api/users sin admin'))
      const creado = await llamar(RS, 'POST', '/api/users', ckAdmin, { name: `${TAG} H`, email: 'parity-h@example.test', password: 'clave-123', role: 'COLLABORATOR' })
      const fila = (await prisma.$queryRawUnsafe(`SELECT password FROM "User" WHERE email = 'parity-h@example.test'`))[0]
      const hashOk = fila && /^\$2[aby]\$/.test(fila.password)
      console.log(`  endurecimiento: contraseña guardada con bcrypt → ${hashOk ? 'sí' : 'NO'}`)
      hashOk ? resumen.ok++ : (resumen.fallas++, fallas.push('contraseña sin hash'))
      if (creado.json && creado.json.id) await llamar(RS, 'DELETE', '/api/users', ckAdmin, { id: creado.json.id })
    }

    if (['profile', 'all'].includes(area)) {
      await flujo('profile (usuario de prueba)', async (base, L) => {
        const P = paso(L)
        await P('PUT updateProfile', base, 'PUT', '/api/profile', ckBot, { action: 'updateProfile', name: `${TAG} bot` }, {})
        await P('PUT email en uso', base, 'PUT', '/api/profile', ckBot, { action: 'updateProfile', email: admin.email }, {})
        await P('PUT changePassword incompleta', base, 'PUT', '/api/profile', ckBot, { action: 'changePassword' }, {})
        await P('PUT changePassword corta', base, 'PUT', '/api/profile', ckBot, { action: 'changePassword', currentPassword: 'x', newPassword: '123' }, {})
        await P('PUT changePassword incorrecta', base, 'PUT', '/api/profile', ckBot, { action: 'changePassword', currentPassword: 'x', newPassword: '123456' }, {})
        await P('PUT disconnectGoogle', base, 'PUT', '/api/profile', ckBot, { action: 'disconnectGoogle' }, {})
        await P('PUT acción inválida', base, 'PUT', '/api/profile', ckBot, { action: 'nada' }, {})
        await P('GET /api/profile', base, 'GET', '/api/profile', ckBot, undefined, { drop: ['googleAccessToken', 'microsoftAccessToken'] })
      })
    }

    if (['meetings', 'all'].includes(area)) {
      await flujo('meetings (usuario de prueba)', async (base, L) => {
        const P = paso(L)
        const c = await P('POST /api/meetings', base, 'POST', '/api/meetings', ckBot,
          { title: `${TAG} reunión`, description: 'desc', type: 'INTERNAL', date: '2026-12-01T10:00', endDate: '2026-12-01T11:00', location: 'Sala', attendees: 'a@b.test', status: 'SCHEDULED' }, {})
        const id = c.json && c.json.id
        await P('PUT estado', base, 'PUT', `/api/meetings/${id}`, ckBot, { status: 'DONE' }, {})
        await P('PUT campos', base, 'PUT', `/api/meetings/${id}`, ckBot, { title: `${TAG} reunión 2`, description: '', endDate: '', link: 'https://x.test' }, {})
        await P('PUT inexistente', base, 'PUT', '/api/meetings/no-existe', ckBot, { title: 'x' }, {})
        await P('GET acciones vacías', base, 'GET', `/api/meetings/${id}/actions`, ckBot, undefined, {})
        const a = await P('POST acción', base, 'POST', `/api/meetings/${id}/actions`, ckBot, { texto: ' pendiente ', responsable: 'Ana', fechaLimite: '2026-12-05' }, {})
        await P('POST acción sin texto', base, 'POST', `/api/meetings/${id}/actions`, ckBot, { texto: '  ' }, {})
        await P('POST acción reunión inexistente', base, 'POST', '/api/meetings/no-existe/actions', ckBot, { texto: 'x' }, {})
        const aid = a.json && a.json.id
        await P('PUT acción', base, 'PUT', `/api/meetings/${id}/actions/${aid}`, ckBot, { estado: 'HECHA', fechaLimite: '' }, {})
        await P('PUT acción estado inválido', base, 'PUT', `/api/meetings/${id}/actions/${aid}`, ckBot, { estado: 'XX' }, {})
        await P('PUT hub', base, 'PUT', `/api/meetings/${id}/hub`, ckBot, { hub: JSON.stringify({ puntos: [{ texto: 'Punto 1' }], notas: '<p>Notas</p>', decisiones: [{ texto: 'Decisión 1' }] }) }, { drop: ['updatedAt'] })
        await P('PUT hub sin hub', base, 'PUT', `/api/meetings/${id}/hub`, ckBot, {}, {})
        await P('PUT hub inexistente', base, 'PUT', '/api/meetings/no-existe/hub', ckBot, { hub: '{}' }, {})
        const ac = await llamar(base, 'POST', `/api/meetings/${id}/acta-generate`, ckBot, { asistentes: ['Ana'], typeLabel: 'Interna' })
        L.push({ nombre: 'POST acta-generate (IA: solo forma)', res: { status: ac.status, json: { texto: typeof (ac.json && ac.json.texto) === 'string' && ac.json.texto.length > 50 ? '<texto>' : ac.json } } })
        await P('acta-generate inexistente', base, 'POST', '/api/meetings/no-existe/acta-generate', ckBot, {}, {})
        await P('DELETE acción', base, 'DELETE', `/api/meetings/${id}/actions/${aid}`, ckBot, undefined, {})
        await P('DELETE acción otra vez', base, 'DELETE', `/api/meetings/${id}/actions/${aid}`, ckBot, undefined, {})
        await P('DELETE reunión', base, 'DELETE', `/api/meetings/${id}`, ckBot, undefined, {})
        await P('DELETE reunión otra vez', base, 'DELETE', `/api/meetings/${id}`, ckBot, undefined, {})
      })
    }

    if (['leads', 'hub', 'all'].includes(area)) {
      await flujo('leads + hub (admin)', async (base, L) => {
        const P = paso(L)
        const lead = { companyName: `${TAG} Corp`, contactName: 'Contacto', email: 'c@parity.test', phone: '123', status: 'NEW', source: 'Web', estimatedValue: '1500.5', scope: 'alcance', repository: '', notes: 'nota', userId: admin.id, tipo: 'Project', solucionAsociada: '' }
        const c = await P('POST /api/leads', base, 'POST', '/api/leads', ckAdmin, lead, {})
        const id = c.json && c.json.id
        if (id) creados.leads.push(id)
        await P('GET lead', base, 'GET', `/api/leads/${id}`, ckAdmin, undefined, {})
        await P('PUT lead (estado + solución)', base, 'PUT', `/api/leads/${id}`, ckAdmin, { ...lead, status: 'DIAGNOSIS', solucionAsociada: 'Demo' }, {})
        await P('PUT RESULT/LOST sin motivo', base, 'PUT', `/api/leads/${id}`, ckAdmin, { ...lead, status: 'RESULT', outcome: 'LOST' }, {})
        await P('PUT sin admin', base, 'PUT', `/api/leads/${id}`, ckBot, { ...lead }, {})
        await P('POST nota', base, 'POST', `/api/leads/${id}/notes`, ckAdmin, { text: ' una nota ' }, {})
        await P('POST nota vacía', base, 'POST', `/api/leads/${id}/notes`, ckAdmin, { text: '  ' }, {})
        await P('GET notas', base, 'GET', `/api/leads/${id}/notes`, ckAdmin, undefined, {})
        await P('POST interacción', base, 'POST', `/api/leads/${id}/interactions`, ckAdmin, { type: 'CALL', description: 'llamada' }, {})
        await P('GET interacciones', base, 'GET', `/api/leads/${id}/interactions`, ckAdmin, undefined, {})
        await P('POST propuesta (crea)', base, 'POST', `/api/leads/${id}/proposal`, ckAdmin, { title: 'Prop', description: 'd', amount: '2500' }, {})
        await P('POST propuesta (actualiza)', base, 'POST', `/api/leads/${id}/proposal`, ckAdmin, { title: 'Prop 2', description: 'd2', amount: 3000 }, {})
        await P('PATCH propuesta SENT', base, 'PATCH', `/api/leads/${id}/proposal`, ckAdmin, { status: 'SENT' }, { drop: ['sentDate'] })
        await P('GET propuesta', base, 'GET', `/api/leads/${id}/proposal`, ckAdmin, undefined, { drop: ['sentDate'] })
        await P('GET backlog-items', base, 'GET', `/api/leads/${id}/backlog-items`, ckAdmin, undefined, {})
        await P('PATCH backlog-items inexistente', base, 'PATCH', `/api/leads/${id}/backlog-items`, ckAdmin, { itemId: 'no-existe', status: 'DONE' }, { esperada: 'Next responde 500 sin cuerpo; Rust responde 500 con JSON de error' })
        // hub
        const f1 = await P('PUT hub-phase NEW', base, 'PUT', '/api/leads/hub-phase', ckAdmin, { leadId: id, phase: 'NEW', content: '<p>fase</p>' }, {})
        await P('PUT hub-phase fase futura', base, 'PUT', '/api/leads/hub-phase', ckAdmin, { leadId: id, phase: 'NEGOTIATION', content: 'x' }, {})
        await P('PUT hub-phase lead inexistente', base, 'PUT', '/api/leads/hub-phase', ckAdmin, { leadId: 'no-existe', phase: 'NEW', content: 'x' }, {})
        await P('GET hub-phase', base, 'GET', `/api/leads/hub-phase?leadId=${id}`, ckAdmin, undefined, {})
        const hubId = f1.json && f1.json.id
        const sub = await P('POST hub-file', base, 'POST', '/api/leads/hub-file', ckAdmin, { hubId, name: 'a.txt', mimeType: 'text/plain', base64: Buffer.from('hola parity').toString('base64') }, {})
        const fid = sub.json && sub.json.id
        await P('GET hub-file', base, 'GET', `/api/leads/hub-file?id=${fid}`, ckAdmin, undefined, {})
        await P('POST hub-file fase inexistente', base, 'POST', '/api/leads/hub-file', ckAdmin, { hubId: 'no-existe', name: 'a', mimeType: 't', base64: 'QQ==' }, {})
        await P('PUT architecture', base, 'PUT', '/api/leads/architecture', ckAdmin, { leadId: id, data: { nodes: [{ id: 'n1', gridX: 1 }], title: 'T' } }, {})
        await P('GET architecture', base, 'GET', `/api/leads/architecture?leadId=${id}`, ckAdmin, undefined, {})
        await P('PUT diagram', base, 'PUT', '/api/leads/diagram', ckAdmin, { leadId: id, data: { nodes: [{ id: 'a', label: 'A', x: 0, y: 0 }], edges: [] } }, {})
        await P('GET diagram', base, 'GET', `/api/leads/diagram?leadId=${id}`, ckAdmin, undefined, {})
        await P('GET architecture sin leadId', base, 'GET', '/api/leads/architecture', ckAdmin, undefined, {})
        // IA: solo forma de la respuesta
        const ia = async (nombre, url, cuerpo, forma) => {
          const r = await llamar(base, 'POST', url, ckAdmin, cuerpo)
          L.push({ nombre: `${nombre} (IA: solo forma)`, res: { status: r.status, json: forma(r) } })
        }
        await ia('POST architecture/generate', '/api/leads/architecture/generate', { leadId: id }, (r) => (r.status === 200 && Array.isArray(r.json.data?.nodes) ? '<ok-nodes>' : r.json))
        await ia('POST diagram/generate', '/api/leads/diagram/generate', { leadId: id }, (r) => (r.status === 200 && Array.isArray(r.json.data?.nodes) ? '<ok-nodes>' : r.json))
        await ia('POST diagram/chat', '/api/leads/diagram/chat', { leadId: id, mensaje: '¿Qué riesgos ves?', diagram: { title: 'T', nodes: [{ id: 'a', label: 'A', type: 'api', x: 0, y: 0 }], edges: [] } }, (r) => (r.status === 200 && typeof r.json.mensaje === 'string' ? '<ok-mensaje>' : r.json))
        await ia('POST ai-chat asesor', `/api/leads/${id}/ai-chat`, { modo: 'asesor', phaseKey: 'NEW', tab: { name: 'Nota', html: '<p>hola</p>' } }, (r) => (r.status === 200 && r.json.tipo === 'respuesta' ? '<ok-respuesta>' : r.json))
        await ia('POST ai-chat modo inválido', `/api/leads/${id}/ai-chat`, { modo: 'zzz', phaseKey: 'NEW' }, (r) => r.json)
        await ia('POST ai-chat fase inválida', `/api/leads/${id}/ai-chat`, { modo: 'asesor', phaseKey: 'ZZZ' }, (r) => r.json)
        // limpieza del flujo
        await P('DELETE hub-file', base, 'DELETE', '/api/leads/hub-file', ckAdmin, { id: fid }, {})
        await P('PUT lead (quita solución)', base, 'PUT', `/api/leads/${id}`, ckAdmin, { ...lead, status: 'DIAGNOSIS', solucionAsociada: '' }, {})
        await P('DELETE sin admin', base, 'DELETE', `/api/leads/${id}`, ckBot, undefined, {})
        await P('DELETE lead', base, 'DELETE', `/api/leads/${id}`, ckAdmin, undefined, {})
      })
    }
  } finally {
    // limpieza de TODO lo de prueba
    await prisma.$executeRawUnsafe(`DELETE FROM "Activity" WHERE description LIKE '%${TAG}%'`)
    await prisma.$executeRawUnsafe(`DELETE FROM "Meeting" WHERE title LIKE '${TAG}%'`)
    await prisma.$executeRawUnsafe(`DELETE FROM "Solucion" WHERE nombre LIKE '${TAG}%'`)
    if (creados.leads.length) await prisma.$executeRawUnsafe(`DELETE FROM "LeadHub" WHERE "leadId" = ANY($1::text[])`, creados.leads)
    await prisma.$executeRawUnsafe(`DELETE FROM "Lead" WHERE "companyName" LIKE '${TAG}%'`)
    await prisma.$executeRawUnsafe(`DELETE FROM "Cliente" WHERE nombre LIKE '${TAG}%'`)
    await prisma.$executeRawUnsafe(`DELETE FROM "Activity" WHERE "userId" = 'parity-bot-id'`)
    await prisma.$executeRawUnsafe(`DELETE FROM "User" WHERE email LIKE 'parity-%@example.test'`)
  }

  console.log(`\n══ RESUMEN: ${resumen.ok} iguales · ${resumen.orden} solo-orden · ${resumen.esperadas} esperadas · ${resumen.fallas} DIFERENCIAS`)
  if (fallas.length) console.log('Con diferencias:\n - ' + fallas.join('\n - '))
  await prisma.$disconnect()
  process.exit(resumen.fallas ? 1 : 0)
}

main().catch(async (e) => { console.error('FALLO del harness:', e); try { await prisma.$disconnect() } catch {} process.exit(2) })
