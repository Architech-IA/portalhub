// Prueba de la lista ligera del backlog y del guardado que NO debe borrar la descripción.
const BASE = process.env.PARITY_RS || 'http://127.0.0.1:3100'
const path = require('path')
const ROOT = '/root/portal-architechia'
const { encode } = require(path.join(ROOT, 'node_modules/next-auth/jwt'))
const { PrismaClient } = require(path.join(ROOT, 'node_modules/@prisma/client'))
let H
let ok = 0, mal = 0
const chk = (n, c, d) => { if (c) { ok++; console.log('  ✓', n) } else { mal++; console.log('  ✗', n, d !== undefined ? '— ' + JSON.stringify(d).slice(0, 200) : '') } }
const j = async (m, u, b) => { const r = await fetch(BASE + u, { method: m, headers: H, body: b === undefined ? undefined : JSON.stringify(b) }); const t = await r.text(); let x; try { x = JSON.parse(t) } catch { x = t } return { s: r.status, x, bytes: Buffer.byteLength(t) } }

;(async () => {
  const prisma = new PrismaClient()
  const [u] = await prisma.$queryRawUnsafe(`SELECT id, name, email, role::text role FROM "User" WHERE role = 'SUPERADMIN' ORDER BY ctid LIMIT 1`)
  await prisma.$disconnect()
  const token = await encode({ token: { name: u.name, email: u.email, sub: u.id, id: u.id, role: u.role, picture: null }, secret: process.env.NEXTAUTH_SECRET, maxAge: 3600 })
  H = { cookie: `__Secure-next-auth.session-token=${token}; next-auth.session-token=${token}`, 'content-type': 'application/json' }
  const larga = 'D'.repeat(400)
  const area = (await j('GET', '/api/areas')).x[0].id
  const c = await j('POST', '/api/backlog', { title: 'PARITY-TEST ligero', description: larga, type: 'TASK', priority: 'LOW', areaId: area })
  const id = c.x.id
  chk('crear ítem de prueba', c.s === 200 && id, c)
  try {
    await j('PATCH', `/api/backlog/${id}/resultado`, { resultado: 'R'.repeat(500) })
    const lig = await j('GET', '/api/backlog?ligero=1')
    const full = await j('GET', '/api/backlog')
    const a = lig.x.find(i => i.id === id), b = full.x.find(i => i.id === id)
    chk('lista ligera: sin description ni resultado', a && !('description' in a) && !('resultado' in a), Object.keys(a || {}))
    chk('lista ligera: extracto de 160 caracteres', a && a.descripcionResumen === larga.slice(0, 160), a && a.descripcionResumen && a.descripcionResumen.length)
    chk('lista completa: igual que antes (con description y resultado)', b && b.description === larga && b.resultado.length === 500 && !('descripcionResumen' in b))
    chk('misma cantidad de ítems', lig.x.length === full.x.length, [lig.x.length, full.x.length])
    console.log(`  · tamaño: completa ${(full.bytes / 1024).toFixed(0)} kB, ligera ${(lig.bytes / 1024).toFixed(0)} kB (${(100 - 100 * lig.bytes / full.bytes).toFixed(0)} % menos)`)

    const uno = await j('GET', `/api/backlog/${id}`)
    chk('GET /api/backlog/{id}: ítem completo', uno.s === 200 && uno.x.description === larga && uno.x.resultado.length === 500, uno.s)
    chk('GET /api/backlog/{id} inexistente → 404', (await j('GET', '/api/backlog/no-existe')).s === 404)

    // Lo que hace la pantalla: devolver el ítem de la lista ligera tal cual con otro estado
    const ped = await j('PUT', `/api/backlog/${id}`, { ...a, status: 'IN_PROGRESS' })
    chk('PUT con el ítem ligero cambia el estado', ped.s === 200 && ped.x.status === 'IN_PROGRESS', ped.s)
    const tras = await j('GET', `/api/backlog/${id}`)
    chk('…y NO borra la descripción', tras.x.description === larga, tras.x.description)
    chk('…ni el resultado', tras.x.resultado && tras.x.resultado.length === 500)
    // Editar de verdad sigue funcionando
    await j('PUT', `/api/backlog/${id}`, { title: 'PARITY-TEST ligero', description: 'nueva', type: 'TASK', priority: 'LOW', status: 'BACKLOG' })
    chk('PUT con description nueva la guarda', (await j('GET', `/api/backlog/${id}`)).x.description === 'nueva')
    await j('PUT', `/api/backlog/${id}`, { title: 'PARITY-TEST ligero', description: '', type: 'TASK', priority: 'LOW', status: 'BACKLOG' })
    chk('PUT con description vacía la limpia (como siempre)', (await j('GET', `/api/backlog/${id}`)).x.description === null)
  } finally {
    await j('DELETE', `/api/backlog/${id}`)
  }
  console.log(`\n══ ligero: ${ok} ok · ${mal} fallas`)
  process.exit(mal ? 1 : 0)
})().catch(e => { console.error(e); process.exit(2) })
