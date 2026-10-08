#!/usr/bin/env node
// Simula lo que hace el navegador al abrir un lead: HTML de la página + dos tandas de APIs en
// paralelo (como las hace page.tsx), por Nginx (público) y directo a Next, para ver dónde se va
// el tiempo.   set -a; . /root/portal-architechia/.env; set +a; node tools/parity/navegacion.js
const path = require('path')
const ROOT = '/root/portal-architechia'
const { encode } = require(path.join(ROOT, 'node_modules/next-auth/jwt'))
const { PrismaClient } = require(path.join(ROOT, 'node_modules/@prisma/client'))
const { execFile } = require('child_process')
const prisma = new PrismaClient()
const N = Number(process.env.NAV_N || 6)

const curl = (args) => new Promise((res) => {
  const t0 = performance.now()
  execFile('curl', ['-s', '-o', '/dev/null', '-m', '60', '-w', '%{http_code}', ...args], (e, out) => res([performance.now() - t0, String(out)]))
})
const med = (xs) => [...xs].sort((a, b) => a - b)[Math.floor(xs.length / 2)]

;(async () => {
  const u = (await prisma.$queryRawUnsafe(`SELECT id, name, email, role::text role FROM "User" WHERE role='SUPERADMIN' ORDER BY ctid LIMIT 1`))[0]
  const lead = (await prisma.$queryRawUnsafe(`SELECT id FROM "Lead" ORDER BY "createdAt" DESC LIMIT 1`))[0].id
  const t = await encode({ token: { name: u.name, email: u.email, sub: u.id, id: u.id, role: u.role }, secret: process.env.NEXTAUTH_SECRET, maxAge: 900 })
  const ck = ['-H', `cookie: __Secure-next-auth.session-token=${t}; next-auth.session-token=${t}`]
  const pub = (ruta) => ['--resolve', 'portal.architechia.co:443:127.0.0.1', ...ck, `https://portal.architechia.co${ruta}`]
  const nxt = (ruta) => [...ck, `http://127.0.0.1:3003${ruta}`]

  const ola = (mk, rutas) => Promise.all(rutas.map((r) => curl(mk(r)))).then((rs) => Math.max(...rs.map((x) => x[0])))
  const A = [`/api/leads/${lead}`, `/api/leads/hub-phase?leadId=${lead}`, '/api/users']
  const B = [`/api/leads/${lead}/interactions`, `/api/leads/${lead}/backlog-items`, `/api/leads/${lead}/proposal`]

  const filas = []
  for (const [nombre, mk] of [['Next (antes)', nxt], ['Por Nginx → Rust (ahora)', pub]]) {
    await curl(mk(`/leads/${lead}/hub`)) // calentamiento (en modo dev la 1.ª compila)
    const html = [], a = [], b = []
    for (let i = 0; i < N; i++) { html.push((await curl(mk(`/leads/${lead}/hub`)))[0]); a.push(await ola(mk, A)); b.push(await ola(mk, B)) }
    filas.push([nombre, med(html), med(a), med(b)])
  }
  console.log(`Abrir un lead (mediana de ${N}), en ms — el navegador espera HTML y luego tanda 1 y tanda 2 de APIs`)
  console.log('camino'.padEnd(28), 'HTML página'.padEnd(13), 'APIs tanda 1'.padEnd(14), 'APIs tanda 2'.padEnd(14), 'TOTAL aprox.')
  for (const [n, h, a, b] of filas) console.log(n.padEnd(28), `${h.toFixed(0)}`.padEnd(13), `${a.toFixed(0)}`.padEnd(14), `${b.toFixed(0)}`.padEnd(14), `${(h + a + b).toFixed(0)}`)

  // Páginas con y sin datos: ¿cuánto es solo "servir la página"?
  const pg = []
  for (const r of ['/leads', '/leads/lista', `/leads/${lead}/hub`, '/login']) { await curl(nxt(r)); const xs = []; for (let i = 0; i < 4; i++) xs.push((await curl(nxt(r)))[0]); pg.push([r.replace(lead, ':id'), med(xs)]) }
  console.log('\nSolo servir la página (HTML), directo a Next, mediana de 4:')
  for (const [r, m] of pg) console.log(' ', r.padEnd(24), `${m.toFixed(0)} ms`)
  await prisma.$disconnect()
})()
