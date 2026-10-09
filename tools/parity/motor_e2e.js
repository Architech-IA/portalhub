#!/usr/bin/env node
/**
 * Prueba de punta a punta del Motor en Rust: crea una tarea LLM mínima, la despacha por portalhub (que la
 * reenvía al motor), espera al worker de Python, y verifica que el motor la cerró (verificador + estado +
 * trazas). Todo con datos "PARITY-TEST", que se borran al final.
 *
 *   set -a; . /root/portal-architechia/.env; set +a
 *   node /root/repos/portalhub/tools/parity/motor_e2e.js
 */
const path = require('path')
const ROOT = '/root/portal-architechia'
const { PrismaClient } = require(path.join(ROOT, 'node_modules/@prisma/client'))
const p = new PrismaClient()
const RS = process.env.PARITY_RS || 'http://127.0.0.1:3100'
const KEY = { 'x-api-key': process.env.INTERNAL_API_KEY, 'content-type': 'application/json' }
const AREA = 'e35ad8bc-eb34-4d84-92b2-2737dc817dcd' // Marketing & Brand (LLM, agente Iris)
const sel = (sql, ...a) => p.$queryRawUnsafe(sql, ...a)

;(async () => {
  const id = 'pt-e2e-' + Date.now()
  await p.$executeRawUnsafe(
    `INSERT INTO "BacklogItem" (id, title, description, type, priority, status, "areaId", "order", "createdAt", "updatedAt")
     VALUES ($1, 'PARITY-TEST e2e del motor', 'Responde únicamente con la palabra OK.', 'TASK', 'LOW', 'BACKLOG', $2, 0, NOW(), NOW())`, id, AREA)
  try {
    const r = await fetch(RS + '/api/executor/dispatch', { method: 'POST', headers: KEY, body: JSON.stringify({ taskId: id }) })
    console.log('dispatch →', r.status, JSON.stringify(await r.json()))
    const doble = await fetch(RS + '/api/executor/dispatch', { method: 'POST', headers: KEY, body: JSON.stringify({ taskId: id }) })
    console.log('dispatch repetido (debe rechazarse) →', doble.status, JSON.stringify(await doble.json()))
    let estado = 'IN_PROGRESS'
    const ini = Date.now()
    while (Date.now() - ini < 8 * 60 * 1000) {
      await new Promise(res => setTimeout(res, 5000))
      const [t] = await sel(`SELECT status, resultado FROM "BacklogItem" WHERE id = $1`, id)
      estado = t.status
      process.stdout.write('.')
      if (!['IN_PROGRESS', 'BACKLOG'].includes(estado)) { console.log('\nestado final:', estado, '| resultado:', (t.resultado || '').slice(0, 200)); break }
    }
    const ex = await sel(`SELECT status, "agentName", "durationMs", left("resultSummary", 120) AS resumen, artifacts->'checklist' AS checklist FROM "TaskExecution" WHERE "backlogItemId" = $1`, id)
    console.log('ejecución:', JSON.stringify(ex))
    const ev = await sel(`SELECT kind, message FROM "TaskExecutionEvent" WHERE "taskId" = $1 ORDER BY "createdAt"`, id)
    console.log('trazas:'); ev.forEach(e => console.log('  -', e.kind, e.message))
    const ok = estado === 'DONE' && ev.length >= 4
    console.log(ok ? '\n✓ E2E del motor OK' : '\n✗ E2E del motor FALLÓ (estado ' + estado + ')')
    process.exitCode = ok ? 0 : 1
  } finally {
    await p.$executeRawUnsafe(`DELETE FROM "TaskExecutionEvent" WHERE "taskId" = $1`, id)
    await p.$executeRawUnsafe(`DELETE FROM "TaskExecution" WHERE "backlogItemId" = $1`, id)
    await p.$executeRawUnsafe(`DELETE FROM "BacklogItem" WHERE id = $1`, id)
    await p.$disconnect()
  }
})().catch(e => { console.error(e); process.exit(2) })
