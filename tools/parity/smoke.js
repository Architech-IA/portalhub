#!/usr/bin/env node
// Prueba de humo por Nginx (HTTPS real): ¿quién contesta, Rust o Next?
// Distingue por una diferencia conocida: GET /api/profile en Next incluye googleAccessToken/
// microsoftAccessToken dentro de `user`; en Rust no.
//   set -a; . /root/portal-architechia/.env; set +a; node tools/parity/smoke.js
const path = require('path')
const ROOT = '/root/portal-architechia'
const { encode } = require(path.join(ROOT, 'node_modules/next-auth/jwt'))
const { PrismaClient } = require(path.join(ROOT, 'node_modules/@prisma/client'))
const prisma = new PrismaClient()
;(async () => {
  const u = (await prisma.$queryRawUnsafe(`SELECT id, name, email, role::text role FROM "User" WHERE role='SUPERADMIN' ORDER BY ctid LIMIT 1`))[0]
  const t = await encode({ token: { name: u.name, email: u.email, sub: u.id, id: u.id, role: u.role }, secret: process.env.NEXTAUTH_SECRET, maxAge: 300 })
  const cookie = `__Secure-next-auth.session-token=${t}; next-auth.session-token=${t}`
  const url = 'https://portal.architechia.co'
  const ip = '127.0.0.1'
  const pedir = async (ruta, ck) => {
    // curl con --resolve: va por el Nginx local con el certificado y el host reales
    const { execFileSync } = require('child_process')
    const args = ['-s', '-m', '30', '--resolve', `portal.architechia.co:443:${ip}`, '-w', '\n%{http_code}', url + ruta]
    if (ck) args.unshift('-H', `cookie: ${ck}`)
    const out = execFileSync('curl', args).toString()
    const i = out.lastIndexOf('\n')
    let json; try { json = JSON.parse(out.slice(0, i)) } catch { json = out.slice(0, i).slice(0, 80) }
    return { status: Number(out.slice(i + 1)), json }
  }
  const quien = (r) => (r.json && r.json.user && 'googleAccessToken' in r.json.user ? 'NEXT' : r.status === 200 ? 'RUST' : '?')
  const p = await pedir('/api/profile', cookie)
  console.log(`GET /api/profile por nginx → ${p.status} servido por: ${quien(p)}`)
  const l = await pedir('/api/leads', cookie)
  console.log(`GET /api/leads   por nginx → ${l.status} (${Array.isArray(l.json) ? l.json.length + ' leads' : JSON.stringify(l.json).slice(0, 60)})`)
  const sin = await pedir('/api/users', null)
  console.log(`GET /api/users SIN sesión   → ${sin.status} (${JSON.stringify(sin.json).slice(0, 60)})`)
  const falsa = await pedir('/api/users', '__Secure-next-auth.session-token=basura')
  console.log(`GET /api/users sesión FALSA → ${falsa.status} (${JSON.stringify(falsa.json).slice(0, 60)})`)
  await prisma.$disconnect()
})()
