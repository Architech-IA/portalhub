#!/usr/bin/env node
// Humo + latencia de las rutas de los bloques 1 y 2 por el camino REAL (Nginx, HTTPS) frente a Next directo.
//   set -a; . /root/portal-architechia/.env; set +a; node tools/parity/humo2.js
const path = require('path')
const ROOT = '/root/portal-architechia'
const { encode } = require(path.join(ROOT, 'node_modules/next-auth/jwt'))
const { PrismaClient } = require(path.join(ROOT, 'node_modules/@prisma/client'))
const { execFileSync } = require('child_process')
const prisma = new PrismaClient()
const med = (xs) => [...xs].sort((a, b) => a - b)[Math.floor(xs.length / 2)]

;(async () => {
  const u = (await prisma.$queryRawUnsafe(`SELECT id, name, email, role::text role FROM "User" WHERE role='SUPERADMIN' ORDER BY ctid LIMIT 1`))[0]
  const t = await encode({ token: { name: u.name, email: u.email, sub: u.id, id: u.id, role: u.role }, secret: process.env.NEXTAUTH_SECRET, maxAge: 600 })
  const cookie = `__Secure-next-auth.session-token=${t}`
  const sol = (await prisma.$queryRawUnsafe(`SELECT id FROM "Solucion" ORDER BY "createdAt" DESC LIMIT 1`))[0]
  const spr = (await prisma.$queryRawUnsafe(`SELECT id FROM "Sprint" ORDER BY "createdAt" DESC LIMIT 1`))[0]
  const directo = async (ruta) => { const t0 = performance.now(); const r = await fetch('http://127.0.0.1:3003' + ruta, { headers: { cookie } }); await r.arrayBuffer(); return [performance.now() - t0, r.status] }
  const nginx = (ruta, max = '30') => {
    const o = execFileSync('curl', ['-s', '-o', '/dev/null', '-m', max, '-w', '%{time_total} %{http_code} %{content_type}', '--resolve', 'portal.architechia.co:443:127.0.0.1', '-H', `cookie: ${cookie}`, 'https://portal.architechia.co' + ruta]).toString().split(' ')
    return [parseFloat(o[0]) * 1000, Number(o[1]), o[2]]
  }
  const rutas = ['/api/backlog', '/api/backlog/epics', '/api/backlog/sprints', '/api/backlog/solution', `/api/backlog/sprint/${spr.id}/graph`, '/api/notifications', '/api/sessions?limit=3', '/api/search?q=Prospector',
    '/api/agents', '/api/agents/status', '/api/council/badge', '/api/reportes', '/api/activities', '/api/areas', '/api/projects', '/api/proposals', '/api/soluciones', `/api/soluciones/${sol.id}`, '/api/iniciativas',
    '/api/productos', '/api/contabilidad/cuentas', '/api/contabilidad/balance', '/api/rrhh/empleados', '/api/finanzas', '/api/inventario', '/api/proveedores', '/api/ordenes', '/api/graph']
  console.log('Ruta'.padEnd(46), 'Next directo'.padEnd(14), 'Nginx (real)'.padEnd(14), 'estado')
  for (const r of rutas) {
    await directo(r); nginx(r)
    const a = [], c = []; let est = 0
    for (let i = 0; i < 5; i++) { a.push((await directo(r))[0]); const n = nginx(r); c.push(n[0]); est = n[1] }
    console.log(r.padEnd(46), `${med(a).toFixed(0)} ms`.padEnd(14), `${med(c).toFixed(0)} ms`.padEnd(14), est)
  }
  console.log('\nRutas que NO deben pasar por Rust (streaming): deben responder text/event-stream desde Next')
  for (const r of ['/api/notifications/sse', '/api/comments/sse']) {
    try { const n = nginx(r, '3'); console.log(r.padEnd(30), n[1], n[2]) } catch (e) { console.log(r.padEnd(30), 'cortado a los 3 s (flujo abierto = correcto)') }
  }
  await prisma.$disconnect()
})()
