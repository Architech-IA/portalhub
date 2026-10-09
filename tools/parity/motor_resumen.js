const path = require('path')
const ROOT = '/root/portal-architechia'
const { encode } = require(path.join(ROOT, 'node_modules/next-auth/jwt'))
const { PrismaClient } = require(path.join(ROOT, 'node_modules/@prisma/client'))
const prisma = new PrismaClient()
;(async () => {
  const u = (await prisma.$queryRawUnsafe(`SELECT id, name, email FROM "User" WHERE role='SUPERADMIN' ORDER BY ctid LIMIT 1`))[0]
  const t = await encode({ token: { name: u.name, email: u.email, sub: u.id, id: u.id, role: 'SUPERADMIN' }, secret: process.env.NEXTAUTH_SECRET, maxAge: 300 })
  const r = await fetch('http://127.0.0.1:3100/api/motor/resumen', { headers: { cookie: `next-auth.session-token=${t}` } })
  const j = await r.json()
  console.log(r.status, JSON.stringify(j.totales), JSON.stringify(j.ejecucion?.harness), 'cartera', j.cartera?.length, 'aprob', j.aprobaciones?.length, 'corriendo', j.ejecucion?.corriendo?.length, 'problemas', j.ejecucion?.problemas?.length, 'atascadas', j.ejecucion?.atascadas?.length, 'costo', JSON.stringify(j.costo).slice(0, 200))
  if (r.status !== 200) console.log(JSON.stringify(j).slice(0, 300))
  await prisma.$disconnect()
})()
