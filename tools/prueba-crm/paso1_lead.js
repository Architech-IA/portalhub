// Paso 1 de la prueba del CRM: crea el lead de prueba como lo hace «Nuevo lead» en Oficina > Motor (POST /api/leads).
const path = require('path')
const ROOT = '/root/portal-architechia'
const { encode } = require(path.join(ROOT, 'node_modules/next-auth/jwt'))
const { PrismaClient } = require(path.join(ROOT, 'node_modules/@prisma/client'))
const prisma = new PrismaClient()
;(async () => {
  const u = (await prisma.$queryRawUnsafe(`SELECT id, name, email FROM "User" WHERE role='SUPERADMIN' ORDER BY ctid LIMIT 1`))[0]
  const t = await encode({ token: { name: u.name, email: u.email, sub: u.id, id: u.id, role: 'SUPERADMIN' }, secret: process.env.NEXTAUTH_SECRET, maxAge: 3600 })
  const cookie = `__Secure-next-auth.session-token=${t}; next-auth.session-token=${t}`
  const r = await fetch('http://127.0.0.1:3100/api/leads', {
    method: 'POST', headers: { 'content-type': 'application/json', cookie },
    body: JSON.stringify({
      companyName: 'PRUEBA · Distribuidora Andina SAS', contactName: 'Marta Quintero (ficticia)', email: 'marta@distribuidora-andina.example', phone: '3000000000',
      source: 'Directo', userId: u.id, estimatedValue: 18000000, solucionAsociada: 'Project', status: 'NEW',
      scope: 'CRM a la medida para un equipo comercial de 8 vendedores: empresas y contactos, pipeline de oportunidades por etapas (tablero tipo kanban), actividades con recordatorios y un tablero de KPIs. Hoy trabajan con Excel y WhatsApp.',
      notes: 'LEAD DE PRUEBA del recorrido idea → MVP → proyecto → entrega (octubre 2026). Cliente ficticio; no contactar.',
    }),
  })
  const j = await r.json()
  console.log(r.status, JSON.stringify({ leadId: j.id, solucionId: j.solucionId, empresa: j.companyName }))
  await prisma.$disconnect()
})()
