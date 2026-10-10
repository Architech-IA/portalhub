// Herramienta de la prueba del CRM: opera el proyecto como un administrador desde la línea de comandos.
//   node crm.js estado                      fase actual, puerta, actividades y «dónde está»
//   node crm.js marcar [nota]               marca TODOS los criterios de la fase actual (como lo haría una persona)
//   node crm.js avanzar [nota]              aprueba la puerta de la fase actual
//   node crm.js ganado [nota]               puerta de negociación: venta ganada
//   node crm.js tareas                      tareas del proyecto en el Backlog
const fs = require('fs')
const path = require('path')
const ROOT = '/root/portal-architechia'
const { encode } = require(path.join(ROOT, 'node_modules/next-auth/jwt'))
const { PrismaClient } = require(path.join(ROOT, 'node_modules/@prisma/client'))
const prisma = new PrismaClient()
const IDS = JSON.parse(fs.readFileSync('/root/crm_ids.json', 'utf8'))
const BASE = 'http://127.0.0.1:3100'
;(async () => {
  const u = (await prisma.$queryRawUnsafe(`SELECT id, name, email FROM "User" WHERE role='SUPERADMIN' ORDER BY ctid LIMIT 1`))[0]
  const t = await encode({ token: { name: u.name, email: u.email, sub: u.id, id: u.id, role: 'SUPERADMIN' }, secret: process.env.NEXTAUTH_SECRET, maxAge: 3600 })
  const cookie = `__Secure-next-auth.session-token=${t}; next-auth.session-token=${t}`
  const call = async (m, r, b) => {
    const x = await fetch(BASE + r, { method: m, headers: { 'content-type': 'application/json', cookie }, body: b === undefined ? undefined : JSON.stringify(b) })
    return { status: x.status, j: await x.json().catch(() => ({})) }
  }
  const sol = IDS.solucionId
  const cmd = process.argv[2] || 'estado'
  const nota = process.argv[3]
  if (cmd === 'estado') {
    const { j } = await call('GET', `/api/proyectos/${sol}/fases`)
    const f = j.fases.find(x => x.clave === j.faseActual)
    console.log(`Estado: ${j.estado} · fase actual: ${f.numero}. ${f.nombre} (${f.bloque}) · lead: ${j.lead?.status}${j.lead?.outcome ? '/' + j.lead.outcome : ''}`)
    console.log(`Puerta (${f.puerta.aprobador}): ${f.puerta.criterios.filter(c => c.ok).length}/${f.puerta.criterios.length}`)
    f.puerta.criterios.forEach(c => console.log(`   [${c.ok ? 'x' : ' '}] ${c.texto}`))
    console.log('Actividades:')
    f.actividades.items.forEach(a => console.log(`   - [${a.tipo}] ${a.titulo} → ${a.status ?? 'sin crear'} ${a.taskCode ?? ''}`))
    console.log('Dónde está:')
    f.recursos.forEach(r => console.log(`   · ${r.etiqueta}: ${r.estado} (${r.nivel})`))
  } else if (cmd === 'marcar') {
    const { j } = await call('GET', `/api/proyectos/${sol}/fases`)
    const f = j.fases.find(x => x.clave === j.faseActual)
    for (let i = 0; i < f.puerta.criterios.length; i++) if (!f.puerta.criterios[i].ok) await call('POST', `/api/proyectos/${sol}/fases/criterio`, { fase: f.clave, indice: i, ok: true })
    console.log(`criterios marcados en ${f.clave}: ${f.puerta.criterios.length}`)
  } else if (cmd === 'avanzar') {
    const r = await call('POST', `/api/proyectos/${sol}/fases/avanzar`, { nota: nota || 'Prueba del CRM' })
    console.log(r.status, JSON.stringify(r.j))
  } else if (cmd === 'ganado') {
    const r = await call('POST', `/api/proyectos/${sol}/fases/avanzar`, { resultado: 'GANADO', nota: nota || 'Prueba del CRM: venta ganada' })
    console.log(r.status, JSON.stringify(r.j))
  } else if (cmd === 'correr') {
    // Despacha por el Motor la tarea cuyo título contiene el texto dado y espera el resultado
    const rows = await prisma.$queryRawUnsafe(`SELECT id, "taskCode", title, status FROM "BacklogItem" WHERE "solucionId" = $1 AND title ILIKE $2 ORDER BY "createdAt" DESC LIMIT 1`, sol, '%' + process.argv[3] + '%')
    if (!rows.length) { console.log('no hay tarea que coincida'); return }
    const tarea = rows[0]
    console.log('Tarea:', tarea.title, '·', tarea.status)
    const d = await fetch('http://127.0.0.1:3101/api/executor/dispatch', { method: 'POST', headers: { 'x-api-key': process.env.INTERNAL_API_KEY ?? '', 'content-type': 'application/json' }, body: JSON.stringify({ taskId: tarea.id }) })
    console.log('dispatch', d.status, JSON.stringify(await d.json().catch(() => ({}))).slice(0, 200))
    const t0 = Date.now()
    let st = 'IN_PROGRESS'
    while (Date.now() - t0 < 9 * 60_000) {
      const [r] = await prisma.$queryRawUnsafe(`SELECT status FROM "BacklogItem" WHERE id = $1`, tarea.id)
      st = r.status
      if (['DONE', 'FAILED', 'BLOCKED', 'CANCELLED'].includes(st)) break
      await new Promise(r => setTimeout(r, 4000))
    }
    const [b] = await prisma.$queryRawUnsafe(`SELECT status, resultado FROM "BacklogItem" WHERE id = $1`, tarea.id)
    const [e] = await prisma.$queryRawUnsafe(`SELECT "durationMs", artifacts FROM "TaskExecution" WHERE "backlogItemId" = $1 ORDER BY "startedAt" DESC LIMIT 1`, tarea.id)
    const uso = e?.artifacts?.usage
    console.log(`Resultado: ${b.status} · ${Math.round((e?.durationMs ?? 0) / 1000)}s${uso ? ' · ' + uso.total_tokens + ' tokens' : ''}`)
    console.log((b.resultado ?? '').slice(0, +(process.argv[4] || 900)))
  } else if (cmd === 'propuesta') {
    // Guarda como propuesta del lead el borrador que escribió el agente (con una corrección de la persona que revisa) y la marca enviada
    const [b] = await prisma.$queryRawUnsafe(`SELECT resultado FROM "BacklogItem" WHERE "solucionId" = $1 AND title ILIKE '%Redactar el borrador de la propuesta%' ORDER BY "createdAt" DESC LIMIT 1`, sol)
    const texto = (b.resultado ?? '').replace('Mover de etapa = arrastrar la tarjeta.', 'Mover de etapa con un botón en cada tarjeta.')
    const g = await call('POST', `/api/leads/${IDS.leadId}/proposal`, { title: 'Propuesta CRM a la medida — Distribuidora Andina SAS (v1)', description: texto, amount: 18000000, status: 'DRAFT' })
    console.log('guardada', g.status, JSON.stringify(g.j).slice(0, 120))
    const e = await call('PATCH', `/api/leads/${IDS.leadId}/proposal`, { status: 'SENT' })
    console.log('enviada', e.status, JSON.stringify(e.j).slice(0, 120))
  } else if (cmd === 'adjuntar') {
    // Sube un archivo como adjunto del proyecto (informes de QA, actas...). Uso: adjuntar <ruta> <nombre que se mostrará>
    const buf = fs.readFileSync(process.argv[3])
    const fd = new FormData()
    fd.append('file', new Blob([buf], { type: 'text/plain' }), process.argv[4] || path.basename(process.argv[3]))
    const x = await fetch(BASE + `/api/proyectos/${sol}/adjuntos`, { method: 'POST', headers: { cookie }, body: fd })
    console.log('adjunto', x.status, JSON.stringify(await x.json().catch(() => ({}))).slice(0, 160))
  } else if (cmd === 'tareas') {
    const rows = await prisma.$queryRawUnsafe(`SELECT "taskCode", status, left(title, 90) t FROM "BacklogItem" WHERE "solucionId" = $1 ORDER BY "createdAt"`, sol)
    rows.forEach(r => console.log(`${r.status.padEnd(12)} ${r.taskCode ?? '-'}  ${r.t}`))
  }
  await prisma.$disconnect()
})()
