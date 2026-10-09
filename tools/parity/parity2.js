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

const SIN_CUERPO = { esperada: 'Next responde 500 sin cuerpo; Rust responde 500 con JSON de error' }
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
const RE_FECHA = /^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(\.\d+)?(Z|\+00:00)$/
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
// ═══════════ Bloques 1 y 2 (backlog, rutas pequeñas, gestión, administración) ═══════════
// Cada base (Next / Rust) trabaja con sus PROPIOS datos de prueba; el código de solución difiere
// ("PTSN" / "PTSR") y se normaliza a "CODE" antes de comparar.
const CODIGO = { [NEXT]: 'PTSN', [RS]: 'PTSR' }
const SUFIJO = { [NEXT]: 'N', [RS]: 'R' }
const sel = (sql, ...p) => prisma.$queryRawUnsafe(sql, ...p)
const run = (sql, ...p) => prisma.$executeRawUnsafe(sql, ...p)
const quitaCodigo = (x, base) => JSON.parse(JSON.stringify(x === undefined ? null : x).split(CODIGO[base]).join('CODE'))

async function flujo2(nombre, fn) {
  console.log(`\n── flujo: ${nombre}`)
  const resN = [], resR = []
  await fn(NEXT, resN)
  await fn(RS, resR)
  const n = Math.max(resN.length, resR.length)
  for (let i = 0; i < n; i++) {
    const a = resN[i], b = resR[i]
    if (!a || !b) { resumen.fallas++; fallas.push(`${nombre} paso ${i}`); console.log(`  ✗ paso ${i}: falta respuesta`); continue }
    comparar(a.nombre, { status: a.res.status, json: quitaCodigo(a.res.json, NEXT) }, { status: b.res.status, json: quitaCodigo(b.res.json, RS) }, { mask: true, drop: a.drop, esperada: a.esperada })
  }
}
const paso2 = (lista) => async (nombre, ...args) => { const o = args.pop(); const res = await llamar(...args); lista.push({ nombre, res, ...o }); return res }
const ESP_NEXT_500 = { esperada: 'Next responde 500 sin cuerpo; Rust 500 con JSON de error' }

async function lecturas2(ck) {
  const get = async (nombre, url, o) => comparar(nombre, await llamar(NEXT, 'GET', url, ck), await llamar(RS, 'GET', url, ck), o)
  if (['b1', 'all2'].includes(area)) {
    await get('GET /api/backlog', '/api/backlog')
    await get('GET /api/backlog/epics', '/api/backlog/epics')
    await get('GET /api/backlog/sprints', '/api/backlog/sprints')
    await get('GET /api/backlog/solution', '/api/backlog/solution')
    for (const sp of await sel(`SELECT id FROM "Sprint" ORDER BY "createdAt" DESC LIMIT 3`)) {
      await get(`GET sprint areas ${sp.id.slice(0, 6)}`, `/api/backlog/sprints/${sp.id}/areas`)
      await get(`GET sprint graph ${sp.id.slice(0, 6)}`, `/api/backlog/sprint/${sp.id}/graph`)
    }
    await get('GET sprint areas inexistente', '/api/backlog/sprints/no-existe/areas')
    await get('GET sprint graph inexistente', '/api/backlog/sprint/no-existe/graph')
    for (const it of await sel(`SELECT bi.id FROM "BacklogItem" bi WHERE EXISTS (SELECT 1 FROM "TaskExecution" t WHERE t."backlogItemId" = bi.id) ORDER BY bi."updatedAt" DESC LIMIT 3`)) {
      await get(`GET executions ${it.id.slice(0, 6)}`, `/api/backlog/${it.id}/executions`)
      await get(`GET logs ${it.id.slice(0, 6)}`, `/api/backlog/logs?itemId=${it.id}`)
    }
    await get('GET logs sin itemId', '/api/backlog/logs')
  }
  if (['b2', 'all2'].includes(area)) {
    const real = (await sel(`SELECT id FROM "User" WHERE role='SUPERADMIN' ORDER BY ctid LIMIT 1`))[0].id
    await get('GET /api/notifications', '/api/notifications')
    await get('GET /api/notifications?userId', `/api/notifications?userId=${real}`)
    await get('GET /api/sessions?limit=5', '/api/sessions?limit=5')
    await get('GET /api/search', '/api/search?q=Prospector')
    await get('GET /api/search corto', '/api/search?q=a')
    await get('GET /api/agents', '/api/agents')
    for (const a of await sel(`SELECT slug FROM "Agent" LIMIT 2`)) await get(`GET agent ${a.slug}`, `/api/agents/${a.slug}`)
    await get('GET agent inexistente', '/api/agents/no-existe')
    await get('GET /api/council/badge', '/api/council/badge')
    await get('GET /api/user-stats', `/api/user-stats?userId=${real}`)
    await get('GET /api/user-stats sin id', '/api/user-stats')
    await get('GET /api/user-stats inexistente', '/api/user-stats?userId=no-existe')
    await get('GET /api/reportes', '/api/reportes')
    await get('GET /api/activities', '/api/activities')
    await get('GET /api/graph', '/api/graph')
    await get('GET /api/projects', '/api/projects')
    await get('GET /api/areas', '/api/areas')
    for (const a of await sel(`SELECT slug FROM "Area" ORDER BY "createdAt" LIMIT 3`)) await get(`GET area activity ${a.slug}`, `/api/areas/${a.slug}/activity`)
    await get('GET area activity inexistente', '/api/areas/no-existe/activity')
    for (const c of await sel(`SELECT "entityType" t, "entityId" i FROM "Comment" WHERE "parentId" IS NULL LIMIT 2`)) await get(`GET comments ${c.t}`, `/api/comments?entityType=${c.t}&entityId=${c.i}`)
    await get('GET comments vacío', '/api/comments')
  }
  if (['g', 'all2'].includes(area)) {
    await get('GET /api/proposals', '/api/proposals')
    for (const p of await sel(`SELECT id FROM "Proposal" ORDER BY "createdAt" DESC LIMIT 2`)) {
      await get(`GET proposal ${p.id.slice(0, 6)}`, `/api/proposals?id=${p.id}`)
      await get(`GET proposal tasks ${p.id.slice(0, 6)}`, `/api/proposals/${p.id}/tasks`)
    }
    await get('GET proposal inexistente', '/api/proposals?id=no-existe')
    await get('GET /api/soluciones', '/api/soluciones')
    await get('GET /api/soluciones?tipo=INTERN', '/api/soluciones?tipo=INTERN')
    for (const so of await sel(`SELECT id FROM "Solucion" ORDER BY "createdAt" DESC LIMIT 3`)) {
      await get(`GET solucion ${so.id.slice(0, 6)}`, `/api/soluciones/${so.id}`)
      await get(`GET hitos ${so.id.slice(0, 6)}`, `/api/hitos?solucionId=${so.id}`)
      await get(`GET riesgos ${so.id.slice(0, 6)}`, `/api/riesgos?solucionId=${so.id}`)
    }
    await get('GET solucion inexistente', '/api/soluciones/no-existe')
    await get('GET hitos sin solucion', '/api/hitos')
    await get('GET /api/iniciativas', '/api/iniciativas')
    await get('GET /api/iniciativas/delete-requests', '/api/iniciativas/delete-requests')
    await get('GET /api/productos', '/api/productos')
  }
  if (['t3', 'all2'].includes(area)) {
    await get('GET /api/apps', '/api/apps')
    await get('GET /api/apps?status=DRAFT', '/api/apps?status=DRAFT')
    await get('GET /api/apps?q=a', '/api/apps?q=a')
    for (const a of await sel('SELECT id, slug FROM "AppInstance" ORDER BY "createdAt" DESC LIMIT 2')) {
      await get('GET app ' + a.slug, '/api/apps/' + a.id)
      await get('GET app by-slug ' + a.slug, '/api/apps/by-slug/' + a.slug)
    }
    await get('GET app inexistente', '/api/apps/no-existe')
    await get('GET app by-slug inexistente', '/api/apps/by-slug/no-existe')
    await get('GET /api/app-types', '/api/app-types')
    await get('GET /api/eventos-app', '/api/eventos-app')
    await get('GET /api/eventos-app?take=3', '/api/eventos-app?take=3')
    await get('GET /api/metrics/display', '/api/metrics/display', { drop: ['ts'] })
    await get('GET /api/public-summary', '/api/public-summary', { drop: ['updated_at'] })
    await get('GET /api/vps/history', '/api/vps/history')
    await get('GET /api/vps/logs', '/api/vps/logs')
    await get('GET /api/vps2/history', '/api/vps2/history')
    await get('GET /api/vps2/logs', '/api/vps2/logs')
    await get('GET /api/vps/stats', '/api/vps/stats', { esperada: 'métricas en vivo: cambian entre una llamada y otra' })
    await get('GET /api/orion/messages', '/api/orion/messages')
    await get('GET /api/orion/messages?areaId', '/api/orion/messages?areaId=947ca771-fe9e-4c3f-bfea-2ef2e27986c6')
    await get('GET /api/council/proposals', '/api/council/proposals')
    await get('GET /api/council/proposals?status=PENDING', '/api/council/proposals?status=PENDING')
    for (const p of await sel('SELECT id FROM "CouncilProposal" ORDER BY "createdAt" DESC LIMIT 3')) {
      await get('GET proposal ' + p.id.slice(0, 6), '/api/council/proposals/' + p.id)
      await get('GET proposal messages ' + p.id.slice(0, 6), '/api/council/proposals/' + p.id + '/messages')
      await get('GET proposal votes ' + p.id.slice(0, 6), '/api/council/proposals/' + p.id + '/votes')
    }
    await get('GET council proposal inexistente', '/api/council/proposals/no-existe')
    await get('GET /api/council/status', '/api/council/status', { drop: ['key', 'latency'] })
    await get('GET /api/council/actions', '/api/council/actions')
    await get('GET /api/council/triggers', '/api/council/triggers')
    await get('GET /api/council/traces', '/api/council/traces')
    await get('GET /api/prospecting/stats', '/api/prospecting/stats', { drop: ['last30'] })
    await get('GET /api/prospecting/autocomplete?q=bo', '/api/prospecting/autocomplete?q=bo')
    await get('GET /api/prospecting/autocomplete?q=b', '/api/prospecting/autocomplete?q=b')
    await get('GET /api/status', '/api/status', { drop: ['latency', 'status'] })
    comparar('POST /api/prospecting/search (sin clave de Places)', await llamar(NEXT, 'POST', '/api/prospecting/search', ck, { city: 'Bogotá', category: 'cafe' }), await llamar(RS, 'POST', '/api/prospecting/search', ck, { city: 'Bogotá', category: 'cafe' }))
    comparar('GET whatsapp (pública)', await llamar(NEXT, 'GET', '/api/council/triggers/whatsapp', undefined), await llamar(RS, 'GET', '/api/council/triggers/whatsapp', undefined))
  }
  if (['a', 'all2'].includes(area)) {
    for (const u of ['/api/contabilidad/asientos', '/api/contabilidad/balance', '/api/contabilidad/balance?desde=2026-01-01&hasta=2026-12-31', '/api/contabilidad/cuentas', '/api/contabilidad/movimientos',
      '/api/rrhh/empleados', '/api/rrhh/nomina', '/api/rrhh/vacaciones', '/api/finanzas', '/api/inventario', '/api/proveedores', '/api/ordenes']) await get(`GET ${u}`, u)
    for (const a of await sel(`SELECT id FROM "AsientoContable" ORDER BY "createdAt" DESC LIMIT 2`)) await get(`GET asiento ${a.id.slice(0, 6)}`, `/api/contabilidad/asientos/${a.id}`)
    await get('GET asiento inexistente', '/api/contabilidad/asientos/no-existe')
  }
}

async function limpiar2() {
  const ids = (await sel(`SELECT id FROM "BacklogItem" WHERE title LIKE '${TAG}%'`)).map((x) => x.id)
  if (ids.length) {
    await run(`DELETE FROM "TaskExecution" WHERE "backlogItemId" = ANY($1::text[])`, ids)
    await run(`DELETE FROM "BacklogItemLog" WHERE "itemId" = ANY($1::text[])`, ids)
  }
  await run(`DELETE FROM "BacklogItem" WHERE title LIKE '${TAG}%'`)
  await run(`DELETE FROM "SprintArea" WHERE "sprintId" IN (SELECT id FROM "Sprint" WHERE name LIKE '${TAG}%')`)
  await run(`DELETE FROM "Sprint" WHERE name LIKE '${TAG}%'`)
  await run(`DELETE FROM "Epic" WHERE name LIKE '${TAG}%'`)
  await run(`DELETE FROM "Solucion" WHERE nombre LIKE '${TAG}%'`)
  await run(`DELETE FROM "OrionLog" WHERE message LIKE '%${TAG}%' OR "backlogItemTitle" LIKE '${TAG}%'`)
  await run(`DELETE FROM "Notification" WHERE title LIKE '${TAG}%'`)
  await run(`DELETE FROM "SessionLog" WHERE details LIKE '${TAG}%'`)
  await run(`DELETE FROM "Comment" WHERE text LIKE '${TAG}%'`)
  await run(`DELETE FROM "Agent" WHERE slug LIKE 'parity-test-%'`)
  await run(`DELETE FROM "Milestone" WHERE name LIKE '${TAG}%'`)
  await run(`DELETE FROM "ProjectUser" WHERE "projectId" IN (SELECT id FROM "Project" WHERE name LIKE '${TAG}%')`)
  await run(`DELETE FROM "Iniciativa" WHERE nombre LIKE '${TAG}%'`)
  await run(`DELETE FROM "Project" WHERE name LIKE '${TAG}%'`)
  await run(`DELETE FROM "Proposal" WHERE title LIKE '${TAG}%'`)
  await run(`DELETE FROM "Producto" WHERE nombre LIKE '${TAG}%'`)
  await run(`DELETE FROM "LineaAsiento" WHERE "asientoId" IN (SELECT id FROM "AsientoContable" WHERE descripcion LIKE '${TAG}%')`)
  await run(`DELETE FROM "AsientoContable" WHERE descripcion LIKE '${TAG}%'`)
  await run(`DELETE FROM "CuentaContable" WHERE nombre LIKE '${TAG}%'`)
  await run(`DELETE FROM "MovimientoBancario" WHERE descripcion LIKE '${TAG}%'`)
  await run(`DELETE FROM "EmpleadoRRHH" WHERE nombre LIKE '${TAG}%'`)
  await run(`DELETE FROM "RegistroFinanciero" WHERE concepto LIKE '${TAG}%'`)
  await run(`DELETE FROM "Activo" WHERE nombre LIKE '${TAG}%'`)
  await run(`DELETE FROM "OrdenCompra" WHERE concepto LIKE '${TAG}%'`)
  await run(`DELETE FROM "Proveedor" WHERE nombre LIKE '${TAG}%'`)
  await run(`DELETE FROM "Lead" WHERE "companyName" LIKE '${TAG}%'`)
  await run(`DELETE FROM "Cliente" WHERE nombre LIKE '${TAG}%'`)
  await run(`DELETE FROM "AppInstance" WHERE name LIKE '${TAG}%'`)
  await run(`DELETE FROM "AppEvento" WHERE "appSlug" LIKE 'parity-test-%'`)
  await run(`DELETE FROM "CouncilProposal" WHERE title LIKE '${TAG}%'`)
  await run(`DELETE FROM "AgentConversation" WHERE "channelId" = 'parity-bot-id'`)
  await run(`DELETE FROM "Activity" WHERE description LIKE '%${TAG}%' OR "userId" = 'parity-bot-id'`)
  await run(`DELETE FROM "User" WHERE email LIKE 'parity-%@example.test'`)
}

async function main() {
  const admin = (await sel(`SELECT id, name, email, role::text role FROM "User" WHERE role = 'SUPERADMIN' ORDER BY ctid LIMIT 1`))[0]
  const ck = await cookie(admin)
  await limpiar2()
  await run(`INSERT INTO "User" (id, name, email, password, role, "updatedAt") VALUES ('parity-bot-id', '${TAG} bot', 'parity-bot@example.test', 'x', 'COLLABORATOR', NOW())`)
  const bot = 'parity-bot-id'
  console.log(`Paridad bloques 1-2 Next(${NEXT}) ↔ Rust(${RS}) — área: ${area}`)
  await lecturas2(ck)

  try {
    const areas = (await sel(`SELECT id FROM "Area" ORDER BY "createdAt" LIMIT 2`)).map((a) => a.id)

    if (['b1', 'all2'].includes(area)) {
      await flujo2('backlog', async (base, L) => {
        const P = paso2(L), b = SUFIJO[base], cod = CODIGO[base]
        const solId = `ptsol-${b}`, epId = `ptepic-${b}`
        await run(`INSERT INTO "Solucion" (id, "solucionCode", nombre, tipo, "updatedAt") VALUES ($1, $2, $3, 'PROJECT', NOW())`, solId, cod, `${TAG} Sol`)
        await run(`INSERT INTO "Epic" (id, name, "solucionId", "updatedAt") VALUES ($1, $2, $3, NOW())`, epId, `${TAG} Epic`, solId)
        const sp = await P('POST sprint', base, 'POST', '/api/backlog/sprints', ck, { name: `${TAG} Sprint`, goal: 'g', startDate: '2026-10-01', endDate: '2026-10-14', solucionId: solId, epicId: epId, responsibleName: 'Alguien' }, {})
        const sid = sp.json && sp.json.id
        await P('POST sprint 2 (sin épica)', base, 'POST', '/api/backlog/sprints', ck, { name: `${TAG} Sprint B`, solucionId: solId }, {})
        await P('PUT sprint estado', base, 'PUT', '/api/backlog/sprints', ck, { id: sid, status: 'ACTIVE' }, {})
        const ids = []
        for (let i = 1; i <= 3; i++) {
          const r = await P(`POST item ${i}`, base, 'POST', '/api/backlog', ck, { title: `${TAG} T${i}`, description: i === 2 ? '' : 'd', type: 'TASK', priority: 'HIGH', points: '3', solucionId: solId, sprintId: sid }, {})
          ids.push(r.json && r.json.id)
        }
        await P('POST item sin sprint', base, 'POST', '/api/backlog', ck, { title: `${TAG} Suelto`, type: 'TASK', priority: 'MEDIUM' }, {})
        await P('POST item sin priority', base, 'POST', '/api/backlog', ck, { title: `${TAG} SinPrio`, type: 'TASK' }, { esperada: 'Next exige priority (500); Rust usa MEDIUM' })
        await P('POST item sin type', base, 'POST', '/api/backlog', ck, { title: `${TAG} SinType` }, { esperada: 'Next exige type (500); Rust lo completa con TASK' })
        await P('POST item sin título', base, 'POST', '/api/backlog', ck, { description: 'x' }, ESP_NEXT_500)
        await P('POST item Orion', base, 'POST', '/api/backlog', ck, { title: `${TAG} Orion`, type: 'TASK', assigneeName: 'Orion', priority: 'LOW' }, {})
        await P('PUT item estado', base, 'PUT', `/api/backlog/${ids[0]}`, ck, { title: `${TAG} T1`, status: 'IN_PROGRESS', priority: 'HIGH', type: 'TASK', points: 5, solucionId: solId, sprintId: sid }, {})
        await P('PUT item hecho + resultado', base, 'PUT', `/api/backlog/${ids[0]}`, ck, { title: `${TAG} T1`, status: 'DONE', resultado: 'listo', fechaEjecucion: '2026-10-05T10:00:00.000Z', description: 'nueva', sprintId: sid }, {})
        await P('PUT item quitar sprint', base, 'PUT', `/api/backlog/${ids[2]}`, ck, { title: `${TAG} T3`, sprintId: null, assigneeName: 'Pepe' }, {})
        await P('PUT item inexistente', base, 'PUT', '/api/backlog/no-existe', ck, { title: 'x' }, ESP_NEXT_500)
        await P('POST reorder', base, 'POST', '/api/backlog/reorder', ck, { sprintId: sid, orderedIds: [ids[1], ids[0]] }, {})
        await P('POST reorder inválido', base, 'POST', '/api/backlog/reorder', ck, { sprintId: sid }, {})
        await P('PUT sprint edit', base, 'PUT', '/api/backlog/sprints/edit', ck, { id: sid, name: `${TAG} Sprint`, goal: 'nuevo', startDate: '2026-10-02', epicId: epId, solucionId: solId, responsibleName: 'Otro' }, {})
        await P('PUT sprint edit (cambia épica)', base, 'PUT', '/api/backlog/sprints/edit', ck, { id: sid, name: `${TAG} Sprint`, solucionId: solId }, {})
        await P('GET sprint areas', base, 'GET', `/api/backlog/sprints/${sid}/areas`, ck, undefined, {})
        await P('PUT sprint areas', base, 'PUT', `/api/backlog/sprints/${sid}/areas`, ck, { ownerAreaId: areas[0], participantAreaIds: [areas[1]] }, {})
        await P('PUT sprint areas vacío', base, 'PUT', `/api/backlog/sprints/${sid}/areas`, ck, { ownerAreaId: null }, {})
        await P('PUT sprint areas inexistente', base, 'PUT', '/api/backlog/sprints/no-existe/areas', ck, {}, {})
        await P('POST log', base, 'POST', '/api/backlog/logs', ck, { itemId: ids[0], fromStatus: 'BACKLOG', toStatus: 'DONE', note: 'ok' }, {})
        await P('GET logs', base, 'GET', `/api/backlog/logs?itemId=${ids[0]}`, ck, undefined, {})
        await P('PATCH resultado', base, 'PATCH', `/api/backlog/${ids[1]}/resultado`, ck, { resultado: 'texto' }, {})
        await P('POST execution RUNNING', base, 'POST', `/api/backlog/${ids[1]}/executions`, ck, { agentId: 'ag', agentName: 'Agente', status: 'RUNNING', contextUsed: 'ctx' }, {})
        await P('POST execution DONE', base, 'POST', `/api/backlog/${ids[1]}/executions`, ck, { agentId: 'ag', agentName: 'Agente', status: 'DONE', resultSummary: 'hecho', artifacts: { checklist: [{ criterion: 'c', passed: true, reason: 'r' }] }, durationMs: 1200 }, {})
        await P('GET executions', base, 'GET', `/api/backlog/${ids[1]}/executions`, ck, undefined, {})
        await P('GET sprint graph', base, 'GET', `/api/backlog/sprint/${sid}/graph`, ck, undefined, {})
        await P('PUT épica', base, 'PUT', '/api/backlog/epics', ck, { id: epId, name: `${TAG} Epic editada`, description: 'x', startDate: '2026-10-01' }, {})
        await P('DELETE épica sin id', base, 'DELETE', '/api/backlog/epics', ck, undefined, {})
        await P('DELETE épica', base, 'DELETE', `/api/backlog/epics?id=${epId}`, ck, undefined, {})
        await P('DELETE item', base, 'DELETE', `/api/backlog/${ids[2]}`, ck, undefined, {})
        await P('DELETE item otra vez', base, 'DELETE', `/api/backlog/${ids[2]}`, ck, undefined, ESP_NEXT_500)
      })
    }

    if (['b2', 'all2'].includes(area)) {
      await flujo2('rutas pequeñas', async (base, L) => {
        const P = paso2(L), b = SUFIJO[base]
        await run(`DELETE FROM "Notification" WHERE "userId" = $1`, bot)
        await run(`DELETE FROM "Activity" WHERE "userId" = $1`, bot)
        const nt = await P('POST notification', base, 'POST', '/api/notifications', ck, { userId: bot, title: `${TAG} N`, message: 'm', link: '/x' }, {})
        await P('PATCH notification leída', base, 'PATCH', '/api/notifications', ck, { id: nt.json && nt.json.id, read: true }, {})
        await P('GET notifications del bot', base, 'GET', `/api/notifications?userId=${bot}`, ck, undefined, {})
        await P('PATCH notificaciones (todas)', base, 'PATCH', '/api/notifications', ck, { userId: bot }, { esperada: 'Next falla (lee el cuerpo dos veces); Rust lo corrige' })
        await P('POST session', base, 'POST', '/api/sessions', ck, { userId: bot, email: 'parity-bot@example.test', action: 'LOGOUT', details: `${TAG} d` }, { 'x-forwarded-for': '9.9.9.9', 'user-agent': 'parity' }, {})
        await P('POST agente', base, 'POST', '/api/agents', ck, { slug: `parity-test-ag`, name: `${TAG} Ag`, role: 'r', area: 'a', personality: 'p', taskTypes: ['x', 'y'], repos: [] }, {})
        await P('POST agente incompleto', base, 'POST', '/api/agents', ck, { slug: 'x' }, {})
        await P('GET agente', base, 'GET', `/api/agents/parity-test-ag`, ck, undefined, {})
        await P('PUT agente', base, 'PUT', `/api/agents/parity-test-ag`, ck, { name: `${TAG} Ag2`, taskTypes: ['z'], llmModel: 'm' }, {})
        await P('DELETE agente (inactivar)', base, 'DELETE', `/api/agents/parity-test-ag`, ck, undefined, {})
        await P('DELETE agente inexistente', base, 'DELETE', '/api/agents/parity-test-no', ck, undefined, ESP_NEXT_500)
        await run(`DELETE FROM "Agent" WHERE slug = 'parity-test-ag'`)
        const cm = await P('POST comentario', base, 'POST', '/api/comments', ck, { text: `${TAG} c`, entityType: 'parity', entityId: `e-${b}`, userId: bot }, {})
        await P('POST respuesta', base, 'POST', '/api/comments', ck, { text: `${TAG} r`, entityType: 'parity', entityId: `e-${b}`, userId: bot, parentId: cm.json && cm.json.id }, {})
        await P('GET comentarios', base, 'GET', `/api/comments?entityType=parity&entityId=e-${b}`, ck, undefined, {})
        await P('POST actividad', base, 'POST', '/api/activities', ck, { type: 'NOTE_ADDED', description: `${TAG} act`, entityType: 'parity', entityId: 'x', userId: bot }, {})
        await P('GET búsqueda', base, 'GET', `/api/search?q=${encodeURIComponent(TAG)}`, ck, undefined, {})
        await P('GET user-stats bot', base, 'GET', `/api/user-stats?userId=${bot}`, ck, undefined, {})
        await P('PATCH área vacío', base, 'PATCH', `/api/areas/${areas[0]}`, ck, {}, {})
        await P('PATCH área inexistente', base, 'PATCH', '/api/areas/no-existe', ck, {}, {})
        const proj = `ptproj-${b}`
        await run(`INSERT INTO "Project" (id, name, description, "updatedAt") VALUES ($1, $2, 'd', NOW())`, proj, `${TAG} Proj`)
        const m = await P('POST hito', base, 'POST', '/api/milestones', ck, { name: `${TAG} Hito`, projectId: proj, dueDate: '2026-11-01' }, {})
        await P('POST hito incompleto', base, 'POST', '/api/milestones', ck, { name: 'x' }, {})
        await P('PATCH hito completado', base, 'PATCH', `/api/milestones/${m.json && m.json.id}`, ck, { status: 'COMPLETED' }, {})
        await P('PATCH hito pendiente', base, 'PATCH', `/api/milestones/${m.json && m.json.id}`, ck, { status: 'IN_PROGRESS' }, {})
        await P('DELETE hito', base, 'DELETE', `/api/milestones/${m.json && m.json.id}`, ck, undefined, {})
        const ld = `ptlead-${b}`
        await run(`INSERT INTO "Cliente" (id, nombre, industria, contacto, email, pais, "updatedAt") VALUES ($1, '${TAG} Cli', 'i', 'c', 'c@c.test', 'CO', NOW())`, 'ptcli-' + b)
        await run(`INSERT INTO "Lead" (id, "companyName", "contactName", email, phone, status, source, "estimatedValue", "userId", "clienteId", "updatedAt") VALUES ($1, $2, 'c', 'p@p.test', '1', 'NEW', 'x', 10, $3, $4, NOW())`, ld, `${TAG} Lead`, bot, 'ptcli-' + b)
        await P('PATCH pipeline RESULT/WON', base, 'PATCH', `/api/pipeline/${ld}`, ck, { status: 'RESULT', outcome: 'WON' }, {})
        await P('PATCH pipeline NEGOTIATION', base, 'PATCH', `/api/pipeline/${ld}`, ck, { status: 'NEGOTIATION', outcome: 'WON' }, {})
        await P('PATCH pipeline inexistente', base, 'PATCH', '/api/pipeline/no-existe', ck, { status: 'NEW' }, {})
        await run(`DELETE FROM "Lead" WHERE id = $1`, ld)
        await run(`DELETE FROM "Cliente" WHERE id = $1`, "ptcli-" + b)
        await run(`DELETE FROM "Milestone" WHERE "projectId" = $1`, proj)
        await run(`DELETE FROM "Project" WHERE id = $1`, proj)
      })
    }

    if (['g', 'all2'].includes(area)) {
      await flujo2('gestión', async (base, L) => {
        const P = paso2(L), b = SUFIJO[base]
        const pr = await P('POST propuesta', base, 'POST', '/api/proposals', ck, { title: `${TAG} Prop`, description: 'd', status: 'DRAFT', amount: '1500.5', userId: bot, sentDate: '2026-10-01' }, {})
        const pid = pr.json && pr.json.id
        await P('POST propuesta (estado por defecto)', base, 'POST', '/api/proposals', ck, { title: `${TAG} Prop2`, description: 'd', amount: 'x', userId: bot }, {})
        await P('GET propuesta por id', base, 'GET', `/api/proposals?id=${pid}`, ck, undefined, {})
        await P('PUT propuesta', base, 'PUT', `/api/proposals/${pid}`, ck, { title: `${TAG} Prop`, description: 'nueva', status: 'SENT', amount: 99, userId: bot }, {})
        await P('PUT propuesta mismo estado', base, 'PUT', `/api/proposals/${pid}`, ck, { title: `${TAG} Prop`, description: 'otra', status: 'SENT', amount: 99, userId: bot }, {})
        await P('POST tarea propuesta', base, 'POST', `/api/proposals/${pid}/tasks`, ck, { title: 'tarea' }, {})
        const ts = await P('GET tareas', base, 'GET', `/api/proposals/${pid}/tasks`, ck, undefined, {})
        const tid = ts.json && ts.json[0] && ts.json[0].id
        await P('PATCH tarea', base, 'PATCH', `/api/proposals/${pid}/tasks/${tid}`, ck, { completed: true }, {})
        await P('DELETE tarea', base, 'DELETE', `/api/proposals/${pid}/tasks/${tid}`, ck, undefined, {})
        await P('DELETE propuesta', base, 'DELETE', `/api/proposals/${pid}`, ck, undefined, {})
        await P('DELETE propuesta otra vez', base, 'DELETE', `/api/proposals/${pid}`, ck, undefined, { esperada: 'Next 500 (sin cuerpo útil)' })

        const solId = `ptsol2-${b}`
        await run(`INSERT INTO "Solucion" (id, "solucionCode", nombre, tipo, "updatedAt") VALUES ($1, $2, $3, 'PROJECT', NOW())`, solId, CODIGO[base], `${TAG} Sol`)
        await P('GET solucion', base, 'GET', `/api/soluciones/${solId}`, ck, undefined, {})
        await P('PUT solucion', base, 'PUT', `/api/soluciones/${solId}`, ck, { nombre: `${TAG} Sol`, descripcion: 'x', tipo: 'DEMO', estado: 'PAUSADO', valorEstimado: '1200', prd: '{"a":1}', planTrabajo: '' }, {})
        await P('PUT solucion inexistente', base, 'PUT', '/api/soluciones/no-existe', ck, { nombre: 'x', tipo: 'DEMO' }, {})
        const ht = await P('POST hito', base, 'POST', '/api/hitos', ck, { solucionId: solId, titulo: ' Hito 1 ', fechaComprometida: '2026-12-01' }, {})
        await P('POST hito incompleto', base, 'POST', '/api/hitos', ck, { titulo: 'x' }, {})
        await P('PUT hito', base, 'PUT', `/api/hitos/${ht.json && ht.json.id}`, ck, { estado: 'CUMPLIDO', fechaReal: '2026-12-02', descripcion: '' }, {})
        await P('GET hitos', base, 'GET', `/api/hitos?solucionId=${solId}`, ck, undefined, {})
        await P('DELETE hito', base, 'DELETE', `/api/hitos/${ht.json && ht.json.id}`, ck, undefined, {})
        await P('PUT hito inexistente', base, 'PUT', '/api/hitos/no-existe', ck, { estado: 'x' }, {})
        const rs = await P('POST riesgo', base, 'POST', '/api/riesgos', ck, { solucionId: solId, titulo: 'Riesgo 1', severidad: 'ALTA' }, {})
        await P('PUT riesgo', base, 'PUT', `/api/riesgos/${rs.json && rs.json.id}`, ck, { estado: 'MITIGADO', mitigacion: 'm', responsable: '' }, {})
        await P('GET riesgos', base, 'GET', `/api/riesgos?solucionId=${solId}`, ck, undefined, {})
        await P('DELETE riesgo', base, 'DELETE', `/api/riesgos/${rs.json && rs.json.id}`, ck, undefined, {})
        await P('DELETE solucion', base, 'DELETE', `/api/soluciones/${solId}`, ck, undefined, {})
        await P('DELETE solucion otra vez', base, 'DELETE', `/api/soluciones/${solId}`, ck, undefined, {})

        const ini = await P('POST iniciativa', base, 'POST', '/api/iniciativas', ck, { nombre: `${TAG} Ini`, descripcion: 'd', tecnologias: ['a', 'b'], costoMin: '10', costoMax: 20, tiempoEstimado: ' 2 semanas ', prioridad: 'ALTA' }, {})
        const iid = ini.json && ini.json.id
        await P('POST iniciativa incompleta', base, 'POST', '/api/iniciativas', ck, { nombre: ' ' }, {})
        await P('PUT iniciativa', base, 'PUT', `/api/iniciativas/${iid}`, ck, { nombre: `${TAG} Ini`, descripcion: 'd2', categoria: 'IA/ML', estado: 'APROBADA', prioridad: 'ALTA', tecnologias: ['z'], costoMin: '', costoMax: null }, {})
        await P('PUT iniciativa (sin estado)', base, 'PUT', `/api/iniciativas/${iid}`, ck, { nombre: `${TAG} Ini`, descripcion: 'd3', categoria: 'IA/ML', prioridad: 'ALTA' }, {})
        await P('POST convertir', base, 'POST', `/api/iniciativas/${iid}/convertir`, ck, {}, {})
        await P('POST convertir otra vez', base, 'POST', `/api/iniciativas/${iid}/convertir`, ck, {}, {})
        await P('POST convertir inexistente', base, 'POST', '/api/iniciativas/no-existe/convertir', ck, {}, {})
        const sl = await P('POST solicitud eliminar', base, 'POST', '/api/iniciativas/delete-requests', ck, { iniciativaId: iid, reason: 'r' }, {})
        await P('POST solicitud duplicada', base, 'POST', '/api/iniciativas/delete-requests', ck, { iniciativaId: iid }, {})
        await P('POST solicitud sin iniciativa', base, 'POST', '/api/iniciativas/delete-requests', ck, {}, {})
        await P('PUT solicitud acción inválida', base, 'PUT', `/api/iniciativas/delete-requests/${sl.json && sl.json.id}`, ck, { action: 'x' }, {})
        await P('PUT solicitud rechazar', base, 'PUT', `/api/iniciativas/delete-requests/${sl.json && sl.json.id}`, ck, { action: 'RECHAZAR' }, {})
        await P('PUT solicitud ya resuelta', base, 'PUT', `/api/iniciativas/delete-requests/${sl.json && sl.json.id}`, ck, { action: 'APROBAR' }, {})
        await P('DELETE iniciativa', base, 'DELETE', `/api/iniciativas/${iid}`, ck, undefined, {})
        await P('DELETE iniciativa otra vez', base, 'DELETE', `/api/iniciativas/${iid}`, ck, undefined, {})

        const pd = await P('POST producto', base, 'POST', '/api/productos', ck, { nombre: `${TAG} Prod`, version: '1', estado: 'ACTIVO', descripcion: 'd', tecnologias: ['a'], caracteristicas: ['c1', 'c2'], icono: 'i', color: 'c' }, {})
        await P('PUT producto', base, 'PUT', `/api/productos/${pd.json && pd.json.id}`, ck, { nombre: `${TAG} Prod`, version: '2', estado: 'ACTIVO', descripcion: 'd', tecnologias: ['b'], caracteristicas: [], icono: 'i', color: 'c' }, {})
        await P('DELETE producto', base, 'DELETE', `/api/productos/${pd.json && pd.json.id}`, ck, undefined, {})
      })
    }

    if (['a', 'all2'].includes(area)) {
      await flujo2('administración', async (base, L) => {
        const P = paso2(L), b = SUFIJO[base]
        const SIN_NUM = { drop: ['numero'] }
        const c1 = await P('POST cuenta 1', base, 'POST', '/api/contabilidad/cuentas', ck, { codigo: `PTCTA1`, nombre: `${TAG} Caja`, tipo: 'ACTIVO', nivel: '2' }, {})
        const c2 = await P('POST cuenta 2', base, 'POST', '/api/contabilidad/cuentas', ck, { codigo: `PTCTA2`, nombre: `${TAG} Ventas`, tipo: 'INGRESO', activa: false }, {})
        await P('PUT cuenta', base, 'PUT', `/api/contabilidad/cuentas/${c1.json && c1.json.id}`, ck, { codigo: `PTCTA1`, nombre: `${TAG} Caja 2`, tipo: 'ACTIVO', subtipo: 'Corriente', nivel: 'x' }, {})
        const a1 = await P('POST asiento', base, 'POST', '/api/contabilidad/asientos', ck, { fecha: '2026-10-01', descripcion: `${TAG} Asiento`, lineas: [{ cuentaId: c1.json.id, debe: '100', haber: 0 }, { cuentaId: c2.json.id, debe: 0, haber: 100 }] }, SIN_NUM)
        await P('POST asiento descuadrado', base, 'POST', '/api/contabilidad/asientos', ck, { fecha: '2026-10-01', descripcion: `${TAG} X`, lineas: [{ cuentaId: c1.json.id, debe: 5, haber: 0 }] }, {})
        await P('GET asiento', base, 'GET', `/api/contabilidad/asientos/${a1.json && a1.json.id}`, ck, undefined, SIN_NUM)
        await P('DELETE asiento', base, 'DELETE', `/api/contabilidad/asientos/${a1.json && a1.json.id}`, ck, undefined, {})
        const mv = await P('POST movimiento', base, 'POST', '/api/contabilidad/movimientos', ck, { fecha: '2026-10-01', descripcion: `${TAG} Mov`, monto: '50', tipo: 'DEBITO', saldo: '1000' }, {})
        await P('PUT movimiento', base, 'PUT', `/api/contabilidad/movimientos/${mv.json && mv.json.id}`, ck, { conciliado: true, descripcion: `${TAG} Mov2`, referencia: 'r' }, {})
        await P('DELETE movimiento', base, 'DELETE', `/api/contabilidad/movimientos/${mv.json && mv.json.id}`, ck, undefined, {})
        await P('DELETE cuenta 1', base, 'DELETE', `/api/contabilidad/cuentas/${c1.json && c1.json.id}`, ck, undefined, {})
        await P('DELETE cuenta 2', base, 'DELETE', `/api/contabilidad/cuentas/${c2.json && c2.json.id}`, ck, undefined, {})

        const em = await P('POST empleado', base, 'POST', '/api/rrhh/empleados', ck, { nombre: `${TAG} Emp`, email: `parity-emp@example.test`, cargo: 'c', departamento: 'd', salarioBase: '1000', fechaIngreso: '2026-01-15' }, {})
        const eid = em.json && em.json.id
        await P('PUT empleado', base, 'PUT', `/api/rrhh/empleados/${eid}`, ck, { nombre: `${TAG} Emp`, email: `parity-emp@example.test`, cargo: 'c2', departamento: 'd', tipo: 'FULL_TIME', estado: 'ACTIVO', salarioBase: 1200, fechaIngreso: '2026-01-15', fechaBaja: '' }, {})
        await P('POST nómina', base, 'POST', '/api/rrhh/nomina', ck, { empleadoId: eid, periodo: '2026-10', salarioBase: '1000', bonos: '50', deducciones: '20' }, {})
        const vc = await P('POST vacaciones', base, 'POST', '/api/rrhh/vacaciones', ck, { empleadoId: eid, desde: '2026-11-01', hasta: '2026-11-05' }, {})
        await P('POST vacaciones con días', base, 'POST', '/api/rrhh/vacaciones', ck, { empleadoId: eid, desde: '2026-11-01', hasta: '2026-11-05', dias: 3, tipo: 'PERMISO' }, {})
        await P('PUT vacaciones', base, 'PUT', `/api/rrhh/vacaciones/${vc.json && vc.json.id}`, ck, { estado: 'APROBADA', aprobadoPor: 'jefe' }, {})
        await P('DELETE empleado (cascada)', base, 'DELETE', `/api/rrhh/empleados/${eid}`, ck, undefined, {})

        const fz = await P('POST finanza', base, 'POST', '/api/finanzas', ck, { fecha: '2026-10-01', tipo: 'ingreso', categoria: 'c', concepto: `${TAG} Fin`, monto: '10.5', proyecto: 'p', responsable: 'r' }, {})
        await P('PUT finanza', base, 'PUT', `/api/finanzas/${fz.json && fz.json.id}`, ck, { fecha: '2026-10-02', tipo: 'gasto', categoria: 'c', concepto: `${TAG} Fin`, monto: 11, moneda: 'EUR', proyecto: 'p', estado: 'pendiente', responsable: 'r' }, {})
        await P('DELETE finanza', base, 'DELETE', `/api/finanzas/${fz.json && fz.json.id}`, ck, undefined, {})

        const ac = await P('POST activo', base, 'POST', '/api/inventario', ck, { nombre: `${TAG} Activo`, tipo: 'HW', valor: '300', fechaAdquisicion: '2026-02-01' }, {})
        await P('PUT activo', base, 'PUT', `/api/inventario/${ac.json && ac.json.id}`, ck, { nombre: `${TAG} Activo`, tipo: 'HW', estado: 'BAJA', valor: 250, notas: 'n' }, {})
        await P('DELETE activo', base, 'DELETE', `/api/inventario/${ac.json && ac.json.id}`, ck, undefined, {})

        const pv = await P('POST proveedor', base, 'POST', '/api/proveedores', ck, { nombre: `${TAG} Prov`, tipo: 'SW', email: 'p@p.test' }, {})
        const pvid = pv.json && pv.json.id
        const od = await P('POST orden', base, 'POST', '/api/ordenes', ck, { concepto: `${TAG} Orden`, monto: '99.9', proveedorId: pvid, fechaVencimiento: '2026-12-31' }, SIN_NUM)
        await P('PUT orden', base, 'PUT', `/api/ordenes/${od.json && od.json.id}`, ck, { concepto: `${TAG} Orden`, monto: 120, estado: 'PAGADA', proveedorId: pvid, fechaPago: '2026-12-01' }, SIN_NUM)
        await P('PUT proveedor', base, 'PUT', `/api/proveedores/${pvid}`, ck, { nombre: `${TAG} Prov`, tipo: 'SW', estado: 'ACTIVO', pais: 'CO' }, {})
        await P('DELETE proveedor con órdenes', base, 'DELETE', `/api/proveedores/${pvid}`, ck, undefined, { esperada: 'falla por la orden asociada en ambos (500)' })
        await P('DELETE orden', base, 'DELETE', `/api/ordenes/${od.json && od.json.id}`, ck, undefined, {})
        await P('DELETE proveedor', base, 'DELETE', `/api/proveedores/${pvid}`, ck, undefined, {})
      })
    }
    if (['t3', 'all2'].includes(area)) {
      const tipo = (await sel('SELECT id FROM "AppType" WHERE "isActive" = true ORDER BY name LIMIT 1'))[0]
      const KEY = { 'x-api-key': process.env.INTERNAL_API_KEY }
      await flujo2('tanda 3: apps, eventos y bitácora de Orión', async (base, L) => {
        const P = paso2(L)
        await run('DELETE FROM "AppInstance" WHERE name LIKE $1', TAG + '%')
        const a = await P('POST app', base, 'POST', '/api/apps', ck, { name: TAG + ' App', appTypeId: tipo.id, description: ' d ' }, {})
        await P('POST app sin tipo', base, 'POST', '/api/apps', ck, { name: 'x' }, {})
        await P('POST app tipo inexistente', base, 'POST', '/api/apps', ck, { name: 'x', appTypeId: 'no-existe' }, {})
        const aid = a.json && a.json.id
        await P('PATCH app', base, 'PATCH', '/api/apps/' + aid, ck, { name: ' ' + TAG + ' App 2 ', status: 'ACTIVE', description: '', config: { a: 1 } }, {})
        await P('PATCH app (sin estado)', base, 'PATCH', '/api/apps/' + aid, ck, { description: 'otra' }, {})
        await P('GET app', base, 'GET', '/api/apps/' + aid, ck, undefined, {})
        await P('DELETE app', base, 'DELETE', '/api/apps/' + aid, ck, undefined, {})
        await P('DELETE app otra vez', base, 'DELETE', '/api/apps/' + aid, ck, undefined, ESP_NEXT_500)
        await P('POST evento sin clave', base, 'POST', '/api/eventos-app', undefined, { appSlug: 'x' }, { esperada: 'Next redirige a /login (middleware); Rust responde 401 JSON' })
        await P('POST evento incompleto', base, 'POST', '/api/eventos-app', undefined, { appSlug: 'x' }, KEY, {})
        await P('POST evento tipo inválido', base, 'POST', '/api/eventos-app', undefined, { appSlug: 'x', tipo: 'NOPE', actorNombre: 'a', actorUsuario: 'b' }, KEY, {})
        await P('POST evento', base, 'POST', '/api/eventos-app', undefined, { appSlug: 'parity-test-app', tipo: 'VISTA', actorNombre: 'A', actorUsuario: 'a', entidad: 'e', metadata: { k: 1 }, ip: '10.0.0.1' }, KEY, {})
        await P('GET eventos del slug', base, 'GET', '/api/eventos-app?appSlug=parity-test-app', ck, undefined, {})
        await run('DELETE FROM "AppEvento" WHERE "appSlug" = $1', 'parity-test-app')
        await P('POST orion/messages sin clave', base, 'POST', '/api/orion/messages', undefined, { message: 'x' }, { esperada: 'Next redirige a /login (middleware); Rust responde 401 JSON' })
        await P('POST orion/messages', base, 'POST', '/api/orion/messages', undefined, { message: TAG + ' log', actionType: 'INFO', metadata: { a: 1 } }, KEY, {})
        await P('POST orion/messages sin mensaje', base, 'POST', '/api/orion/messages', undefined, {}, KEY, {})
      })

      await flujo2('tanda 3: consejo', async (base, L) => {
        const P = paso2(L)
        const pr = await P('POST propuesta', base, 'POST', '/api/council/proposals', ck, { title: TAG + ' Consejo', description: 'd', items: [{ type: 'task', title: 't1' }], metadata: { x: 1 } }, {})
        const id = pr.json && pr.json.id
        await P('POST propuesta sin título', base, 'POST', '/api/council/proposals', ck, {}, {})
        await P('GET propuesta', base, 'GET', '/api/council/proposals/' + id, ck, undefined, {})
        await P('POST mensaje', base, 'POST', '/api/council/proposals/' + id + '/messages', ck, { agentId: 'agent_orion_001', agentName: 'Orión', agentSlug: 'orion', content: 'hola', round: 1 }, {})
        await P('POST mensaje incompleto', base, 'POST', '/api/council/proposals/' + id + '/messages', ck, { agentId: 'x' }, {})
        await P('GET mensajes', base, 'GET', '/api/council/proposals/' + id + '/messages', ck, undefined, {})
        await P('POST voto', base, 'POST', '/api/council/proposals/' + id + '/votes', ck, { agentId: 'agent_orion_001', agentName: 'Orión', agentSlug: 'orion', vote: true, argument: 'ok', round: 1 }, {})
        await P('POST voto (actualiza)', base, 'POST', '/api/council/proposals/' + id + '/votes', ck, { agentId: 'agent_orion_001', agentName: 'Orión', agentSlug: 'orion', vote: false, argument: 'no', round: 1 }, {})
        await P('POST voto incompleto', base, 'POST', '/api/council/proposals/' + id + '/votes', ck, { agentId: 'x' }, {})
        await P('GET votos', base, 'GET', '/api/council/proposals/' + id + '/votes', ck, undefined, {})
        await P('GET propuesta con mensajes y votos', base, 'GET', '/api/council/proposals/' + id, ck, undefined, {})
        await P('PATCH propuesta', base, 'PATCH', '/api/council/proposals/' + id, ck, { status: 'REVISED', round: 2 }, {})
        await P('PATCH inexistente', base, 'PATCH', '/api/council/proposals/no-existe', ck, { status: 'X' }, {})
        await P('DELETE propuesta', base, 'DELETE', '/api/council/proposals/' + id, ck, undefined, {})
        await P('POST adjust inexistente', base, 'POST', '/api/council/proposals/no-existe/adjust/start', ck, {}, {})
        await P('POST debate inexistente', base, 'POST', '/api/council/proposals/no-existe/debate/start', ck, {}, {})
        await P('POST plan/start inexistente', base, 'POST', '/api/council/proposals/no-existe/plan/start', ck, {}, {})
        await P('POST plan/approve inexistente', base, 'POST', '/api/council/proposals/no-existe/plan/approve', ck, {}, {})
        await P('POST finalize inexistente', base, 'POST', '/api/council/proposals/no-existe/finalize', ck, {}, {})
        await P('POST negotiate inexistente', base, 'POST', '/api/council/proposals/no-existe/negotiate', ck, {}, {})
        await P('POST chat sin mensajes', base, 'POST', '/api/council/chat', ck, {}, {})
        await P('POST extract sin mensajes', base, 'POST', '/api/council/chat/extract', ck, {}, {})
        await P('POST actions approve inválida', base, 'POST', '/api/council/actions/x/zzz', ck, {}, {})
        await P('POST run sin task', base, 'POST', '/api/council/run', ck, {}, {})
      })

      await flujo2('tanda 3: Orión y Prospector', async (base, L) => {
        const P = paso2(L)
        const ckBot = await cookie({ id: bot, name: TAG + ' bot', email: 'parity-bot@example.test', role: 'COLLABORATOR' })
        await run('DELETE FROM "AgentConversation" WHERE "channelId" = $1', bot)
        await run("INSERT INTO \"AgentConversation\" (id, \"agentSlug\", \"channelType\", \"channelId\", messages) VALUES ($1, 'orion', 'hub', $2, $3::jsonb)", 'ptconv-' + SUFIJO[base], bot, JSON.stringify([{ role: 'user', content: 'hola ' + TAG }, { role: 'assistant', content: 'respuesta' }]))
        await P('GET historial orion (bot)', base, 'GET', '/api/orion/chat', ckBot, undefined, {})
        await P('DELETE (cerrar sesión)', base, 'DELETE', '/api/orion/chat', ckBot, undefined, { drop: ['id', 'startedAt', 'endedAt'] })
        const h = await P('GET historial tras cerrar', base, 'GET', '/api/orion/chat', ckBot, undefined, { drop: ['id', 'startedAt', 'endedAt'] })
        const sid = h.json && h.json.sessions && h.json.sessions[0] && h.json.sessions[0].id
        await P('PATCH reabrir sin id', base, 'PATCH', '/api/orion/chat', ckBot, {}, {})
        await P('PATCH reabrir inexistente', base, 'PATCH', '/api/orion/chat', ckBot, { sessionId: 'no-existe' }, {})
        await P('PATCH reabrir sesión', base, 'PATCH', '/api/orion/chat', ckBot, { sessionId: sid }, { drop: ['id', 'startedAt', 'endedAt'] })
        await P('POST chat sin mensaje', base, 'POST', '/api/orion/chat', undefined, {}, {})
        await run('DELETE FROM "AgentConversation" WHERE "channelId" = $1', bot)
        await P('POST convert sin lugares', base, 'POST', '/api/prospecting/convert', ck, {}, {})
        await P('POST convert', base, 'POST', '/api/prospecting/convert', ck, { places: [{ name: TAG + ' Lugar', types: ['restaurant_bar', 'food'], website: 'https://x.test', phone: '123' }, { name: TAG + ' Otro', types: [] }] }, {})
        await P('POST convert repetido', base, 'POST', '/api/prospecting/convert', ck, { places: [{ name: TAG + ' Lugar', types: [] }] }, {})
        await run("DELETE FROM \"Lead\" WHERE \"companyName\" LIKE '" + TAG + "%'")
        await run("DELETE FROM \"Cliente\" WHERE nombre LIKE '" + TAG + "%'")
      })
    }
    if (['t4', 'all2'].includes(area)) {
      await require('./t4')({ flujo2, paso2, ck, run, sel, TAG, SUFIJO, CODIGO, NEXT, RS, llamar, comparar })
    }
  } finally {
    await limpiar2()
  }

  console.log(`\n══ RESUMEN: ${resumen.ok} iguales · ${resumen.orden} solo-orden · ${resumen.esperadas} esperadas · ${resumen.fallas} DIFERENCIAS`)
  if (fallas.length) console.log('Con diferencias:\n - ' + fallas.join('\n - '))
  await prisma.$disconnect()
  process.exit(resumen.fallas ? 1 : 0)
}

main().catch(async (e) => { console.error('FALLO del harness:', e); try { await limpiar2() } catch {} try { await prisma.$disconnect() } catch {} process.exit(2) })
