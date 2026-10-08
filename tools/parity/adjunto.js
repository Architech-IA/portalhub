#!/usr/bin/env node
// ¿Lee Rust el adjunto REAL (PDF) igual que Next? Llama a ai-chat (modo asesor) del lead que
// tiene el archivo en ambos servicios y compara cuántos archivos se leyeron.
const path = require('path')
const ROOT = '/root/portal-architechia'
const { encode } = require(path.join(ROOT, 'node_modules/next-auth/jwt'))
const { PrismaClient } = require(path.join(ROOT, 'node_modules/@prisma/client'))
const prisma = new PrismaClient()
;(async () => {
  const f = (await prisma.$queryRawUnsafe(`SELECT f.name, f.size, h."leadId", h.phase FROM "LeadHubFile" f JOIN "LeadHub" h ON h.id = f."hubId" LIMIT 1`))[0]
  const u = (await prisma.$queryRawUnsafe(`SELECT id, name, email, role::text role FROM "User" WHERE role='SUPERADMIN' ORDER BY ctid LIMIT 1`))[0]
  const t = await encode({ token: { name: u.name, email: u.email, sub: u.id, id: u.id, role: u.role }, secret: process.env.NEXTAUTH_SECRET, maxAge: 600 })
  const cookie = `__Secure-next-auth.session-token=${t}; next-auth.session-token=${t}`
  console.log(`Adjunto: ${f.name.slice(0, 60)} (${f.size} bytes) en fase ${f.phase}`)
  for (const [nombre, base] of [['Next', 'http://127.0.0.1:3003'], ['Rust', 'http://127.0.0.1:3100']]) {
    const r = await fetch(`${base}/api/leads/${f.leadId}/ai-chat`, {
      method: 'POST', headers: { 'content-type': 'application/json', cookie },
      body: JSON.stringify({ modo: 'asesor', phaseKey: f.phase, historial: [{ role: 'user', content: 'Menciona el nombre del documento adjunto y de qué trata, en una frase.' }], tab: { name: 'Nota', html: '' } }),
    })
    const j = await r.json()
    console.log(`${nombre}: ${r.status} contexto=${JSON.stringify(j.contexto)} respuesta="${String(j.mensaje || j.error).replace(/\s+/g, ' ').slice(0, 170)}"`)
  }
  await prisma.$disconnect()
})()
