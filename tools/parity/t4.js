/**
 * Paridad de la tanda 4 (proyectos, soluciones con IA, agentes, disparadores, documentos, ejecutor y SSE).
 * Lo invoca parity2.js (`node parity2.js t4`). Las partes que llaman al modelo de IA o a Docker/git solo se
 * prueban en sus caminos de error: lo que es no determinista o cambia el servidor no se compara.
 */
module.exports = async function tanda4(c) {
  const { flujo2, paso2, ck, run, sel, TAG, SUFIJO, CODIGO, NEXT, RS, llamar, comparar } = c
  const KEY = { 'x-api-key': process.env.INTERNAL_API_KEY }

  // ── Lecturas sobre datos reales ────────────────────────────────────────────────────────────
  const get = async (nombre, url, o) => comparar(nombre, await llamar(NEXT, 'GET', url, ck), await llamar(RS, 'GET', url, ck), o)
  console.log('\n── lecturas: proyectos')
  await get('GET /api/proyectos', '/api/proyectos')
  for (const so of await sel(`SELECT id FROM "Solucion" ORDER BY "updatedAt" DESC LIMIT 2`)) {
    const p = `/api/proyectos/${so.id}`
    await get(`GET proyecto ${so.id.slice(0, 6)}`, p)
    await get('  contexto', p + '/contexto', { drop: ['chars', 'actualizado', 'totalChars'] })
    await get('  memoria', p + '/memoria')
    await get('  adjuntos', p + '/adjuntos')
    await get('  suscripciones', p + '/suscripciones')
    await get('  sprints', p + '/sprints')
    await get('  buscar', p + '/buscar?q=proyecto')
    await get('  buscar corto', p + '/buscar?q=a')
    await get('  deploy estado', p + '/deploy')
    await get('  db estado', p + '/db')
  }
  await get('GET proyecto inexistente', '/api/proyectos/no-existe')
  await get('GET agents/status', '/api/agents/status', { drop: ['latency', 'detail'] })
  await get('GET agents/sage/history', '/api/agents/sage/history')
  await get('GET agents/nexus/history', '/api/agents/nexus/history')
  await get('GET council/trigger/config', '/api/council/trigger/config')

  // ── Proyectos: sesiones, memoria, adjuntos, automatizaciones ──────────────────────────────
  await flujo2('tanda 4: proyectos', async (base, L) => {
    const P = paso2(L), b = SUFIJO[base], cod = CODIGO[base]
    const solId = `pt4sol-${b}`
    await run(`DELETE FROM "Solucion" WHERE id = $1`, solId)
    await run(`INSERT INTO "Solucion" (id, "solucionCode", nombre, tipo, "updatedAt") VALUES ($1, $2, $3, 'PROJECT', NOW())`, solId, cod, `${TAG} Proyecto`)
    const url = (x = '') => `/api/proyectos/${solId}${x}`
    const MASK = {}
    const s1 = await P('POST sesión', base, 'POST', url('/sesiones'), ck, { titulo: ' Mi sesión ', tipo: 'PLANIFICACION' }, MASK)
    const sid = s1.json && s1.json.id
    await P('POST sesión sin cuerpo', base, 'POST', url('/sesiones'), ck, {}, MASK)
    await P('POST sesión tipo inválido', base, 'POST', url('/sesiones'), ck, { tipo: 'X', privada: true }, MASK)
    await P('GET sesión', base, 'GET', url(`/sesiones/${sid}`), ck, undefined, { drop: ['ahora'] })
    await P('GET sesión inexistente', base, 'GET', url('/sesiones/no-existe'), ck, undefined, {})
    await P('PATCH sesión', base, 'PATCH', url(`/sesiones/${sid}`), ck, { titulo: 'Otro título', fuentesExcluidas: ['prd', 'ficha', 'x'] }, MASK)
    await P('PATCH sesión vacío', base, 'PATCH', url(`/sesiones/${sid}`), ck, {}, {})
    await P('POST mensaje vacío', base, 'POST', url(`/sesiones/${sid}/mensajes`), ck, { contenido: '   ' }, {})
    await P('POST bifurcar sin mensajes', base, 'POST', url(`/sesiones/${sid}/bifurcar`), ck, { desdeOrden: 1 }, {})
    await P('POST bifurcar inválido', base, 'POST', url(`/sesiones/${sid}/bifurcar`), ck, {}, {})
    await P('POST plan con pocos mensajes', base, 'POST', url(`/sesiones/${sid}/plan`), ck, {}, {})
    await P('GET contexto de la sesión', base, 'GET', url(`/contexto?sesionId=${sid}`), ck, undefined, { drop: ['chars', 'actualizado', 'totalChars'] })
    await P('GET contexto', base, 'GET', url('/contexto'), ck, undefined, { drop: ['chars', 'actualizado', 'totalChars'] })

    await P('GET memoria vacía', base, 'GET', url('/memoria'), ck, undefined, {})
    await P('PUT memoria sin contenido', base, 'PUT', url('/memoria'), ck, {}, {})
    await P('PUT memoria', base, 'PUT', url('/memoria'), ck, { contenido: '# Memoria\n- decisión 1', nota: 'primera' }, MASK)
    await P('PUT memoria con baseVersion al día', base, 'PUT', url('/memoria'), ck, { contenido: '# Memoria\n- decisión 2', baseVersion: 1 }, MASK)
    await P('PUT memoria con conflicto', base, 'PUT', url('/memoria'), ck, { contenido: 'x', baseVersion: 1 }, {})
    await P('GET memoria', base, 'GET', url('/memoria'), ck, undefined, {})
    await P('GET memoria versión 1', base, 'GET', url('/memoria?version=1'), ck, undefined, {})
    await P('GET memoria versión inexistente', base, 'GET', url('/memoria?version=99'), ck, undefined, {})
    await P('POST propuesta inexistente', base, 'POST', url('/memoria/propuestas/no-existe'), ck, { accion: 'rechazar' }, {})

    // Adjunto (multipart)
    const formulario = () => { const f = new FormData(); f.append('file', new Blob(['hola mundo parity'], { type: 'text/plain' }), 'nota.txt'); f.append('sesionId', sid); return f }
    const subir = async (nombre) => {
      const r = await fetch((base) + url('/adjuntos'), { method: 'POST', headers: { cookie: ck }, body: formulario() })
      let json; try { json = await r.json() } catch { json = null }
      const res = { status: r.status, json }
      L.push({ nombre, res, drop: ['size'] })
      return res
    }
    const ad = await subir('POST adjunto')
    await P('GET adjuntos', base, 'GET', url('/adjuntos'), ck, undefined, {})
    await P('DELETE adjunto', base, 'DELETE', url(`/adjuntos/${ad.json && ad.json.id}`), ck, undefined, {})
    await P('DELETE adjunto otra vez', base, 'DELETE', url(`/adjuntos/${ad.json && ad.json.id}`), ck, undefined, {})

    // Automatizaciones
    await P('POST suscripción tipo inválido', base, 'POST', url('/suscripciones'), ck, { tipo: 'X' }, {})
    const su = await P('POST suscripción', base, 'POST', url('/suscripciones'), ck, { tipo: 'BACKLOG_CAMBIOS', nombre: 'Cambios', intervaloMin: 5 }, MASK)
    await P('POST suscripción duplicada', base, 'POST', url('/suscripciones'), ck, { tipo: 'BACKLOG_CAMBIOS' }, {})
    await P('GET suscripciones', base, 'GET', url('/suscripciones'), ck, undefined, {})
    await P('PATCH suscripción', base, 'PATCH', url(`/suscripciones/${su.json && su.json.id}`), ck, { activa: false, intervaloMin: 15, nombre: 'N' }, MASK)
    await P('PATCH suscripción vacío', base, 'PATCH', url(`/suscripciones/${su.json && su.json.id}`), ck, {}, {})
    await P('POST ejecutar suscripción', base, 'POST', url(`/suscripciones/${su.json && su.json.id}/ejecutar`), ck, {}, {})
    await P('POST ejecutar inexistente', base, 'POST', url('/suscripciones/no-existe/ejecutar'), ck, {}, {})
    await P('POST ejecutar todas sin clave', base, 'POST', '/api/proyectos/suscripciones/ejecutar', ck, {}, {})
    await P('DELETE suscripción', base, 'DELETE', url(`/suscripciones/${su.json && su.json.id}`), ck, undefined, {})

    // Plan → tareas
    await P('POST aplicar plan sin tareas', base, 'POST', url(`/sesiones/${sid}/plan/aplicar`), ck, { tareas: [] }, {})
    await P('POST aplicar plan', base, 'POST', url(`/sesiones/${sid}/plan/aplicar`), ck, { tareas: [{ title: `${TAG} Tarea uno`, description: 'd', priority: 'HIGH', areaSlug: 'dev' }, { title: `${TAG} Tarea dos`, priority: 'X', areaSlug: 'zzz' }, { title: 'ab' }] }, { drop: ['id'] })
    await P('GET sesión tras aplicar', base, 'GET', url(`/sesiones/${sid}`), ck, undefined, { drop: ['ahora'] })
    await P('GET sprints', base, 'GET', url('/sprints'), ck, undefined, {})
    await P('POST cerrar sesión', base, 'POST', url(`/sesiones/${sid}/cerrar`), ck, {}, MASK)
    await P('DELETE sesión', base, 'DELETE', url(`/sesiones/${sid}`), ck, undefined, {})
    await P('DELETE sesión otra vez', base, 'DELETE', url(`/sesiones/${sid}`), ck, undefined, {})
    await run(`DELETE FROM "BacklogItem" WHERE "solucionId" = $1`, solId)
    for (const t of ['ProyectoMensaje', 'ProyectoAdjunto', 'ProyectoSuscripcion', 'ProyectoMemoriaPropuesta', 'ProyectoMemoriaVersion', 'ProyectoMemoria']) {
      await run(`DELETE FROM "${t}" WHERE "${t === 'ProyectoMensaje' ? 'sesionId' : 'solucionId'}" ${t === 'ProyectoMensaje' ? 'IN (SELECT id FROM "ProyectoSesion" WHERE "solucionId" = $1)' : '= $1'}`, solId).catch(() => {})
    }
    await run(`DELETE FROM "ProyectoSesion" WHERE "solucionId" = $1`, solId).catch(() => {})
    await run(`DELETE FROM "Solucion" WHERE id = $1`, solId)
  })

  // ── Soluciones con IA y agentes: solo caminos de error (el modelo no es determinista) ─────
  await flujo2('tanda 4: IA de soluciones y agentes (errores)', async (base, L) => {
    const P = paso2(L)
    await P('prd-generate inexistente', base, 'POST', '/api/soluciones/no-existe/prd-generate', ck, {}, {})
    await P('arquitectura-generate inexistente', base, 'POST', '/api/soluciones/no-existe/arquitectura-generate', ck, {}, {})
    await P('prd-seccion-chat sección desconocida', base, 'POST', '/api/soluciones/no-existe/prd-seccion-chat', ck, { seccionKey: 'zzz' }, {})
    await P('prd-seccion-chat solución inexistente', base, 'POST', '/api/soluciones/no-existe/prd-seccion-chat', ck, { seccionKey: 'problema' }, {})
    await P('agente chat sin mensaje', base, 'POST', '/api/agents/orion/chat', ck, {}, {})
    await P('agente [slug] chat sin mensaje', base, 'POST', '/api/agents/hera/chat', ck, {}, {})
    await P('agente [slug] chat agente inexistente', base, 'POST', '/api/agents/no-existe/chat', ck, { message: 'hola' }, {})
    await P('nexus chat sin mensaje', base, 'POST', '/api/agents/nexus/chat', ck, {}, {})
    await P('sage chat sin mensaje', base, 'POST', '/api/agents/sage/chat', ck, {}, {})
  })

  // ── Disparadores del consejo + alta de épicas/soluciones ─────────────────────────────────
  await flujo2('tanda 4: disparadores, épicas y soluciones', async (base, L) => {
    const P = paso2(L), b = SUFIJO[base]
    await P('PUT trigger config (todo apagado)', base, 'PUT', '/api/council/trigger/config', ck, { PRODUCT: false, PROJECT: false, INTERN: false, PILOT: false, epicTriggerEnabled: false }, {})
    await P('GET trigger config', base, 'GET', '/api/council/trigger/config', ck, undefined, {})
    const e = await P('POST épica', base, 'POST', '/api/backlog/epics', ck, { name: `${TAG} Épica`, description: 'd', priority: 'HIGH', startDate: '2026-10-01', endDate: '2026-12-01' }, {})
    await P('POST épica mínima', base, 'POST', '/api/backlog/epics', ck, { name: `${TAG} Épica mínima` }, {})
    await P('POST épica sin nombre', base, 'POST', '/api/backlog/epics', ck, {}, ESP500)
    const s = await P('POST solución', base, 'POST', '/api/soluciones', ck, { nombre: `${TAG} Solución`, tipo: 'INTERN', descripcion: 'd', valorEstimado: '1500.5', solucionCode: `pt4${b.toLowerCase()}` }, { drop: ['solucionCode'] })
    await P('POST solución con código generado', base, 'POST', '/api/soluciones', ck, { nombre: `${TAG} Otra`, tipo: 'INTERN' }, { drop: ['solucionCode'] })
    await P('POST solución sin nombre', base, 'POST', '/api/soluciones', ck, { tipo: 'INTERN' }, ESP500)
    await P('GET soluciones INTERN', base, 'GET', '/api/soluciones?tipo=INTERN', ck, undefined, { drop: ['solucionCode'] })
    await P('PUT trigger config (defecto)', base, 'PUT', '/api/council/trigger/config', ck, { PRODUCT: true, PROJECT: true, INTERN: false, PILOT: false, epicTriggerEnabled: true }, {})
    await run(`DELETE FROM "Epic" WHERE name LIKE '${TAG}%'`)
    await run(`DELETE FROM "Solucion" WHERE nombre LIKE '${TAG}%'`)
    void e; void s
  })

  // ── Documentos de propuestas ──────────────────────────────────────────────────────────────
  await flujo2('tanda 4: documentos de propuesta', async (base, L) => {
    const P = paso2(L), b = SUFIJO[base]
    const uid = (await sel(`SELECT id FROM "User" ORDER BY ctid LIMIT 1`))[0].id
    await run(`DELETE FROM "Proposal" WHERE id = $1`, `pt4prop-${b}`)
    await run(`INSERT INTO "Proposal" (id, title, description, status, amount, "userId", "updatedAt") VALUES ($1, $2, 'd', 'DRAFT', 10, $3, NOW())`, `pt4prop-${b}`, `${TAG} Propuesta`, uid)
    const u = `/api/proposals/pt4prop-${b}/documents`
    await P('POST documento sin datos', base, 'POST', u, ck, {}, {})
    const d1 = await P('POST documento', base, 'POST', u, ck, { name: 'nota.txt', url: 'data:text/plain;base64,aG9sYQ==', type: 'otro', stage: 'x' }, { drop: ['createdAt'] })
    await P('POST documento que reemplaza', base, 'POST', u, ck, { name: 'nota2.txt', url: 'data:text/plain;base64,aG9sYQ==', replacesId: d1.json && d1.json.id }, { drop: ['createdAt'] })
    await P('POST reemplazo inexistente', base, 'POST', u, ck, { name: 'x', url: 'data:text/plain;base64,aA==', replacesId: 'no-existe' }, {})
    await P('GET documentos', base, 'GET', u, ck, undefined, {})
    await P('GET documentos por etapa', base, 'GET', u + '?stage=x', ck, undefined, {})
    await P('GET documento por id', base, 'GET', u + `?docId=${d1.json && d1.json.id}`, ck, undefined, {})
    await P('GET documento inexistente', base, 'GET', u + '?docId=no-existe', ck, undefined, {})
    await P('DELETE documento', base, 'DELETE', u, ck, { docId: d1.json && d1.json.id }, {})
    await run(`DELETE FROM "ProposalDocument" WHERE "proposalId" = $1`, `pt4prop-${b}`)
    await run(`DELETE FROM "Proposal" WHERE id = $1`, `pt4prop-${b}`)
  })

  // ── Ejecutor (solo lectura y validaciones) ────────────────────────────────────────────────
  await flujo2('tanda 4: ejecutor (validaciones)', async (base, L) => {
    const P = paso2(L), b = SUFIJO[base]
    await P('GET eventos sin taskId', base, 'GET', '/api/executor/event', ck, undefined, {})
    await P('GET eventos', base, 'GET', '/api/executor/event?taskId=no-existe', ck, undefined, {})
    await P('POST evento incompleto', base, 'POST', '/api/executor/event', ck, { taskId: 'x' }, {})
    await P('POST evento kind inválido', base, 'POST', '/api/executor/event', ck, { taskId: 'x', kind: 'zz', message: 'm' }, {})
    await P('POST evento', base, 'POST', '/api/executor/event', ck, { taskId: `pt4task-${b}`, kind: 'info', message: `${TAG} traza` }, {})
    await P('GET eventos del evento', base, 'GET', `/api/executor/event?taskId=pt4task-${b}`, ck, undefined, { drop: ['id', 'createdAt'] })
    await P('POST complete sin cuerpo', base, 'POST', '/api/executor/complete', undefined, {}, KEY, {})
    await P('POST complete status inválido', base, 'POST', '/api/executor/complete', undefined, { taskId: 'x', execId: 'y', status: 'RUNNING' }, KEY, {})
    await P('POST explain-complete sin datos', base, 'POST', '/api/executor/explain-complete', undefined, {}, KEY, {})
    await P('POST plan-complete sin datos', base, 'POST', '/api/executor/plan-complete', undefined, {}, KEY, {})
    await P('POST dispatch sin taskId', base, 'POST', '/api/executor/dispatch', undefined, {}, KEY, {})
    await P('POST dispatch inexistente', base, 'POST', '/api/executor/dispatch', undefined, { taskId: 'no-existe' }, KEY, {})
    await P('POST dispatch-chain vacío', base, 'POST', '/api/executor/dispatch-chain', undefined, { taskIds: [] }, KEY, {})
    await P('POST explain inexistente', base, 'POST', '/api/backlog/task/no-existe/explain', ck, {}, {})
    await P('GET explain sin execId', base, 'GET', '/api/backlog/task/x/explain', ck, undefined, {})
    await P('GET explain inexistente', base, 'GET', '/api/backlog/task/x/explain?execId=no-existe', ck, undefined, {})
    await P('POST plan inexistente', base, 'POST', '/api/backlog/task/no-existe/plan', ck, {}, {})
    await P('GET plan inexistente', base, 'GET', '/api/backlog/task/x/plan?execId=no-existe', ck, undefined, {})
    await P('POST apply-plan sin execId', base, 'POST', '/api/backlog/task/x/apply-plan', ck, {}, {})
    await P('POST apply-plan inexistente', base, 'POST', '/api/backlog/task/x/apply-plan', ck, { execId: 'no-existe' }, {})
    await P('POST reactivar sin bloqueadas', base, 'POST', '/api/backlog/sprint/no-existe/reactivate-blocked', ck, {}, {})
    await P('POST aprobar sprint inexistente', base, 'POST', '/api/backlog/sprints/no-existe/approve', ck, {}, {})
    await P('GET env proyecto inexistente', base, 'GET', '/api/proyectos/no-existe/env', ck, undefined, {})
    await P('POST deploy sin repositorio', base, 'POST', '/api/proyectos/no-existe/deploy', ck, {}, {})
    await P('POST db inexistente', base, 'POST', '/api/proyectos/no-existe/db', ck, {}, {})
    await P('POST migrar inexistente', base, 'POST', '/api/proyectos/no-existe/db/migrar', ck, {}, {})
    await run(`DELETE FROM "TaskExecutionEvent" WHERE "taskId" LIKE 'pt4task-%'`)
  })

  // ── Variables de entorno de un proyecto (archivo) ────────────────────────────────────────
  await flujo2('tanda 4: variables de entorno', async (base, L) => {
    const P = paso2(L), b = SUFIJO[base]
    const solId = `pt4env-${b}`
    await run(`DELETE FROM "Solucion" WHERE id = $1`, solId)
    await run(`INSERT INTO "Solucion" (id, nombre, tipo, "updatedAt") VALUES ($1, $2, 'PROJECT', NOW())`, solId, `${TAG} Env ${b}`)
    const u = `/api/proyectos/${solId}/env`
    await P('GET env vacío', base, 'GET', u, ck, undefined, {})
    await P('POST env incompleto', base, 'POST', u, ck, { nombre: 'A' }, {})
    await P('POST env nombre inválido', base, 'POST', u, ck, { nombre: 'minuscula', valor: '1' }, {})
    await P('POST env', base, 'POST', u, ck, { nombre: 'PT_UNO', valor: 'a=b' }, {})
    await P('POST env otra', base, 'POST', u, ck, { nombre: 'PT_DOS', valor: '2' }, {})
    await P('POST env actualiza', base, 'POST', u, ck, { nombre: 'PT_UNO', valor: 'z' }, {})
    await P('DELETE env sin variable', base, 'DELETE', u, ck, undefined, {})
    await P('DELETE env', base, 'DELETE', u + '?variable=PT_UNO', ck, undefined, {})
    await P('GET env', base, 'GET', u, ck, undefined, {})
    await run(`DELETE FROM "Solucion" WHERE id = $1`, solId)
  })

  // ── SSE: primer evento ───────────────────────────────────────────────────────────────────
  console.log('\n── SSE')
  const primero = async (base, url) => {
    const ctl = new AbortController()
    const t = setTimeout(() => ctl.abort(), 8000)
    try {
      const r = await fetch(base + url, { headers: { cookie: ck }, signal: ctl.signal })
      const lector = r.body.getReader()
      let texto = ''
      while (!texto.includes('\n\n')) { const { value, done } = await lector.read(); if (done) break; texto += Buffer.from(value).toString() }
      ctl.abort()
      return { status: r.status, tipo: r.headers.get('content-type'), json: (() => { try { return JSON.parse(texto.replace(/^data: /, '').trim()) } catch { return texto.slice(0, 100) } })() }
    } catch (e) { return { status: -1, json: String(e).slice(0, 100) } } finally { clearTimeout(t) }
  }
  for (const [nombre, url] of [['SSE notificaciones', '/api/notifications/sse'], ['SSE comentarios', '/api/comments/sse?entityType=x&entityId=y']]) {
    comparar(nombre, await primero(NEXT, url), await primero(RS, url), { mask: true })
  }
}

const ESP500 = { esperada: 'Next responde 500 sin cuerpo (Prisma lanza); Rust 500 con JSON de error' }
