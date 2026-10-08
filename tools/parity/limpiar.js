// Borra cualquier resto de pruebas de paridad (por si un harness anterior se cortó a la mitad).
const path = require('path')
const { PrismaClient } = require(path.join('/root/portal-architechia', 'node_modules/@prisma/client'))
const p = new PrismaClient()
const sql = [
  `DELETE FROM "Activity" WHERE description LIKE '%PARITY-TEST%' OR "userId" = 'parity-bot-id'`,
  `DELETE FROM "Meeting" WHERE title LIKE 'PARITY-TEST%'`,
  `DELETE FROM "Solucion" WHERE nombre LIKE 'PARITY-TEST%'`,
  `DELETE FROM "Lead" WHERE "companyName" LIKE 'PARITY-TEST%'`,
  `DELETE FROM "Cliente" WHERE nombre LIKE 'PARITY-TEST%'`,
  `DELETE FROM "Prospecto" WHERE empresa LIKE 'PARITY-TEST%'`,
  `DELETE FROM "NicheConnection" WHERE "fromId" IN (SELECT id FROM "NicheMarket" WHERE name LIKE 'PARITY-TEST%') OR "toId" IN (SELECT id FROM "NicheMarket" WHERE name LIKE 'PARITY-TEST%')`,
  `DELETE FROM "NicheMarket" WHERE name LIKE 'PARITY-TEST%'`,
  `DELETE FROM "ProspectorResult" WHERE "placeId" LIKE 'parity-%'`,
  `DELETE FROM "User" WHERE email LIKE 'parity-%@example.test'`,
]
;(async () => {
  for (const s of sql) { try { const n = await p.$executeRawUnsafe(s); if (n) console.log(n, '←', s.slice(0, 60)) } catch (e) { console.log('ERR', s.slice(0, 50), e.message.slice(0, 80)) } }
  const r = await p.$queryRawUnsafe(`SELECT (SELECT COUNT(*) FROM "User" WHERE email LIKE 'parity-%')::int usuarios, (SELECT COUNT(*) FROM "Lead" WHERE "companyName" LIKE 'PARITY%')::int leads, (SELECT COUNT(*) FROM "Meeting" WHERE title LIKE 'PARITY%')::int reuniones`)
  console.log('restos:', JSON.stringify(r))
  process.exit(0)
})()
