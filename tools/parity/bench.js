#!/usr/bin/env node
// Latencia Next ↔ Rust sobre las mismas peticiones (lecturas), y comprobación manual de la
// única desviación esperada del dashboard (tendencias).
//   set -a; . /root/portal-architechia/.env; set +a; node tools/parity/bench.js
const path = require('path')
const ROOT = '/root/portal-architechia'
const { encode } = require(path.join(ROOT, 'node_modules/next-auth/jwt'))
const { PrismaClient } = require(path.join(ROOT, 'node_modules/@prisma/client'))
const NEXT = 'http://127.0.0.1:3003', RS = 'http://127.0.0.1:3100'
const N = Number(process.env.BENCH_N || 15)
const prisma = new PrismaClient()

;(async () => {
  const u = (await prisma.$queryRawUnsafe(`SELECT id, name, email, role::text role FROM "User" WHERE role='SUPERADMIN' ORDER BY ctid LIMIT 1`))[0]
  const t = await encode({ token: { name: u.name, email: u.email, sub: u.id, id: u.id, role: u.role }, secret: process.env.NEXTAUTH_SECRET, maxAge: 600 })
  const cookie = `__Secure-next-auth.session-token=${t}; next-auth.session-token=${t}`
  const get = async (base, url) => { const t0 = performance.now(); const r = await fetch(base + url, { headers: { cookie } }); await r.text(); return [performance.now() - t0, r.status] }
  const stats = (xs) => { const s = [...xs].sort((a, b) => a - b); return { med: s[Math.floor(s.length / 2)].toFixed(0), p95: s[Math.floor(s.length * 0.95)].toFixed(0), min: s[0].toFixed(0) } }

  const casos = ['/api/leads', '/api/meetings', '/api/dashboard/personal', '/api/users', '/api/profile']
  const lead = (await prisma.$queryRawUnsafe(`SELECT id FROM "Lead" ORDER BY "createdAt" DESC LIMIT 1`))[0]
  if (lead) casos.push(`/api/leads/${lead.id}`, `/api/leads/hub-phase?leadId=${lead.id}`)
  console.log(`Latencia en ms (mediana / p95 / mínimo), ${N} peticiones por caso, secuenciales, tras 3 de calentamiento`)
  console.log('caso'.padEnd(46), 'Next'.padEnd(18), 'Rust'.padEnd(18), 'mejora')
  for (const c of casos) {
    for (let i = 0; i < 3; i++) { await get(NEXT, c); await get(RS, c) }
    const a = [], b = []
    for (let i = 0; i < N; i++) { a.push((await get(NEXT, c))[0]); b.push((await get(RS, c))[0]) }
    const sa = stats(a), sb = stats(b)
    console.log(c.slice(0, 45).padEnd(46), `${sa.med}/${sa.p95}/${sa.min}`.padEnd(18), `${sb.med}/${sb.p95}/${sb.min}`.padEnd(18), `${(sa.med / sb.med).toFixed(1)}x`)
  }

  // Dashboard: sin caché en ninguno de los dos lados (se compara la 1.ª petición tras 31 s sin pedirlo)
  const d1 = await (await fetch(NEXT + '/api/dashboard', { headers: { cookie } })).json()
  const d2 = await (await fetch(RS + '/api/dashboard', { headers: { cookie } })).json()
  console.log('\nTendencias (Next | Rust):')
  d1.tendencias.forEach((x, i) => console.log(' ', JSON.stringify(x), '|', JSON.stringify(d2.tendencias[i]), JSON.stringify(x) === JSON.stringify(d2.tendencias[i]) ? '' : '  ← difiere'))
  await prisma.$disconnect()
})()
