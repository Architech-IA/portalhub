#!/usr/bin/env node
// Latencia de las llamadas de las pestañas de /leads: Next (directo) vs Rust (directo) vs el
// camino real (Nginx, HTTPS).   set -a; . /root/portal-architechia/.env; set +a; node tools/parity/pestanas.js
const path = require('path')
const ROOT = '/root/portal-architechia'
const { encode } = require(path.join(ROOT, 'node_modules/next-auth/jwt'))
const { PrismaClient } = require(path.join(ROOT, 'node_modules/@prisma/client'))
const { execFileSync } = require('child_process')
const prisma = new PrismaClient()
const med = (xs) => [...xs].sort((a, b) => a - b)[Math.floor(xs.length / 2)]

;(async () => {
  const u = (await prisma.$queryRawUnsafe(`SELECT id, name, email, role::text role FROM "User" WHERE role='SUPERADMIN' ORDER BY ctid LIMIT 1`))[0]
  const t = await encode({ token: { name: u.name, email: u.email, sub: u.id, id: u.id, role: u.role }, secret: process.env.NEXTAUTH_SECRET, maxAge: 300 })
  const cookie = `__Secure-next-auth.session-token=${t}`
  const directo = async (base, ruta) => { const t0 = performance.now(); const r = await fetch(base + ruta, { headers: { cookie } }); await r.arrayBuffer(); return [performance.now() - t0, r.status] }
  const nginx = (ruta) => {
    const o = execFileSync('curl', ['-s', '-o', '/dev/null', '-m', '30', '-w', '%{time_total} %{http_code}', '--resolve', 'portal.architechia.co:443:127.0.0.1', '-H', `cookie: ${cookie}`, 'https://portal.architechia.co' + ruta]).toString().split(' ')
    return [parseFloat(o[0]) * 1000, Number(o[1])]
  }
  console.log('Llamada de cada pestaña — mediana de 8, ms'.padEnd(46), 'Next'.padEnd(8), 'Rust'.padEnd(8), 'Nginx (real)  estado')
  for (const r of ['/api/leads', '/api/users', '/api/clientes', '/api/prospectos', '/api/niches', '/api/prospecting/table']) {
    const a = [], b = [], c = []; let est = 0
    await directo('http://127.0.0.1:3003', r); await directo('http://127.0.0.1:3100', r)
    for (let i = 0; i < 8; i++) { a.push((await directo('http://127.0.0.1:3003', r))[0]); b.push((await directo('http://127.0.0.1:3100', r))[0]); const n = nginx(r); c.push(n[0]); est = n[1] }
    console.log(r.padEnd(46), med(a).toFixed(0).padEnd(8), med(b).toFixed(0).padEnd(8), `${med(c).toFixed(0)}`.padEnd(8), est)
  }
  await prisma.$disconnect()
})()
