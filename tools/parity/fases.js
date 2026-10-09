#!/usr/bin/env node
// Prueba de integración del motor de fases contra el servicio real (Rust :3100) y la base real.
// Crea leads y soluciones de prueba (PARITY-TEST…), recorre el ciclo completo y LIMPIA todo al final
// (incluidas las notificaciones que el motor manda a los administradores).
const path = require('path')
const ROOT = '/root/portal-architechia'
const { encode } = require(path.join(ROOT, 'node_modules/next-auth/jwt'))
const { PrismaClient } = require(path.join(ROOT, 'node_modules/@prisma/client'))
const prisma = new PrismaClient()
const BASE = 'http://127.0.0.1:3100'

let fallos = 0
const ok = (cond, msg, extra) => { if (!cond) fallos++; console.log(`${cond ? '  ok  ' : ' FALLA'} ${msg}${!cond && extra !== undefined ? ' → ' + JSON.stringify(extra).slice(0, 300) : ''}`) }
const q = (sql, ...p) => prisma.$queryRawUnsafe(sql, ...p)

;(async () => {
  const u = (await q(`SELECT id, name, email, role::text role FROM "User" WHERE role='SUPERADMIN' ORDER BY ctid LIMIT 1`))[0]
  const cookieDe = async (role) => {
    const t = await encode({ token: { name: u.name, email: u.email, sub: u.id, id: u.id, role }, secret: process.env.NEXTAUTH_SECRET, maxAge: 900 })
    return `__Secure-next-auth.session-token=${t}; next-auth.session-token=${t}`
  }
  const cSuper = await cookieDe('SUPERADMIN')
  const cAdmin = await cookieDe('ADMIN')
  const cUser = await cookieDe('USER')
  const llamar = async (cookie, metodo, ruta, cuerpo) => {
    const r = await fetch(BASE + ruta, { method: metodo, headers: { 'content-type': 'application/json', cookie }, body: cuerpo === undefined ? undefined : JSON.stringify(cuerpo) })
    return { status: r.status, j: await r.json().catch(() => ({})) }
  }
  const S = (m, r, b) => llamar(cSuper, m, r, b)
  const leads = [], sols = []
  let r0

  const nuevoLead = async (nombre) => {
    const l = await S('POST', '/api/leads', { companyName: nombre, contactName: 'Prueba', email: 'prueba@example.test', source: 'Prueba', userId: u.id, solucionAsociada: 'MVP', scope: 'Prueba del motor de fases', estimatedValue: 1000 })
    const id = l.j.id
    leads.push(id)
    const sol = (await q(`SELECT id FROM "Solucion" WHERE "leadId" = $1`, id))[0]
    sols.push(sol.id)
    return { id, sol: sol.id, put: l, solucionId: l.j.solucionId }
  }
  const lead = (id) => q(`SELECT status::text s, outcome, "lostReason" r, "solucionAsociada" a FROM "Lead" WHERE id = $1`, id).then(r => r[0])
  const fases = (sol) => S('GET', `/api/proyectos/${sol}/fases`).then(r => r.j)
  const marcarTodos = async (sol, fase, n) => { for (let i = 0; i < n; i++) await S('POST', `/api/proyectos/${sol}/fases/criterio`, { fase, indice: i, ok: true }) }

  try {
    console.log('— A: ciclo completo (preventa → venta → ejecución → cierre)')
    const A = await nuevoLead('PARITY-TEST Fases A')
    ok(A.put.status === 200, 'lead y solución de prueba creados', A.put)
    ok(A.put.status === 200 && A.solucionId === A.sol, 'crear el lead devuelve su solucionId', A.put)
    let g = await fases(A.sol)
    ok(g.iniciado === true && g.faseActual === 'identificacion', 'el lead nació con el motor iniciado en identificación', g)
    ok((await q(`SELECT nombre, tipo FROM "Solucion" WHERE id = $1`, A.sol))[0].nombre === 'PARITY-TEST Fases A — MVP', 'la solución se llama «empresa — solución asociada»')
    let lp = (await S('GET', '/api/proyectos')).j.find(x => x.id === A.sol)
    ok(lp?.fase?.numero === 1 && lp.fase.nombre === 'Identificación' && lp.fase.bloque === 'PREVENTA' && lp.fase.puerta.total === 3 && lp.fase.puerta.ok === 0, 'la lista de proyectos trae la fase y la puerta', lp?.fase)
    ok((await llamar(cUser, 'POST', `/api/proyectos/${A.sol}/fases/iniciar`, {})).status === 403, 'un usuario normal no puede iniciar (403)')
    ok((await S('POST', `/api/proyectos/${A.sol}/fases/iniciar`, {})).status === 400, 'iniciar dos veces da 400')
    r0 = await S('PUT', `/api/leads/${A.id}`, { status: 'NEW', solucionAsociada: '', scope: 'Prueba del motor de fases' })
    ok(r0.status === 200 && (await q(`SELECT COUNT(*)::int n FROM "Solucion" WHERE id = $1`, A.sol))[0].n === 1 && (await fases(A.sol)).iniciado, 'vaciar «solución asociada» ya no borra la solución ni el motor')
    await S('PUT', `/api/leads/${A.id}`, { status: 'NEW', solucionAsociada: 'MVP', scope: 'Prueba del motor de fases' })
    g = await fases(A.sol)
    ok(g.iniciado && g.faseActual === 'identificacion' && g.fases.length === 12, 'iniciado en identificación con 12 fases', g.faseActual)
    ok(g.fases[0].actividades.total === 2 && g.fases[0].actividades.items.every(i => i.creada), 'actividades de la fase 1 creadas')
    ok((await q(`SELECT COUNT(*)::int n FROM "BacklogItem" WHERE "solucionId" = $1`, A.sol))[0].n === 2, 'dos tareas en el Backlog')
    const it = (await q(`SELECT title, type, status, "areaId" FROM "BacklogItem" WHERE "solucionId" = $1 ORDER BY title`, A.sol))[0]
    ok(it.title.startsWith('[Fase 1 · Identificación]') && it.status === 'BACKLOG' && !!it.areaId, 'tarea con prefijo de fase, estado BACKLOG y área', it)

    let r = await S('POST', `/api/proyectos/${A.sol}/fases/avanzar`, {})
    ok(r.status === 400 && r.j.faltan?.length === 3, 'avanzar sin criterios da 400 con la lista de faltantes', r)
    r = await S('POST', `/api/proyectos/${A.sol}/fases/criterio`, { fase: 'contacto', indice: 0, ok: true })
    ok(r.status === 400, 'no se marcan criterios de otra fase')
    await marcarTodos(A.sol, 'identificacion', 3)
    g = await fases(A.sol)
    ok(g.fases[0].puerta.cumplida && g.fases[0].puerta.criterios[0].por === u.name, 'criterios marcados con quién y cuándo')
    r = await S('POST', `/api/proyectos/${A.sol}/fases/avanzar`, { nota: 'ok' })
    ok(r.status === 200 && r.j.faseActual === 'contacto', 'avanza a contacto', r)
    lp = (await S('GET', '/api/proyectos')).j.find(x => x.id === A.sol)
    ok(lp.fase.numero === 2 && lp.fase.nombre === 'Contacto', 'la lista sigue a la fase al avanzar', lp.fase)
    ok((await lead(A.id)).s === 'CONTACTED', 'el lead pasó a CONTACTED')
    ok((await q(`SELECT COUNT(*)::int n FROM "BacklogItem" WHERE "solucionId" = $1`, A.sol))[0].n === 4, 'tareas de contacto creadas (4 en total)')

    r = await S('PUT', `/api/leads/${A.id}`, { status: 'DIAGNOSIS', solucionAsociada: 'MVP', scope: 'Prueba del motor de fases' })
    g = await fases(A.sol)
    ok(g.faseActual === 'diagnostico', 'cambiar el lead a DIAGNOSIS mueve la fase', g.faseActual)
    await S('PUT', `/api/leads/${A.id}`, { status: 'CONTACTED', solucionAsociada: 'MVP', scope: 'Prueba del motor de fases' })
    g = await fases(A.sol)
    ok(g.faseActual === 'contacto' && g.historial.some(h => h.accion === 'SINCRONIZADO'), 'volver el lead a CONTACTED retrocede la fase y queda en el historial', g.faseActual)

    r = await llamar(cAdmin, 'POST', `/api/proyectos/${A.sol}/fases/avanzar`, { forzar: true })
    ok(r.status === 403, 'un ADMIN no puede forzar (403)', r)
    for (const esperada of ['diagnostico', 'demo', 'propuesta', 'negociacion']) {
      r = await S('POST', `/api/proyectos/${A.sol}/fases/avanzar`, { forzar: true, nota: 'prueba' })
      ok(r.status === 200 && r.j.faseActual === esperada, `forzar → ${esperada}`, r)
    }
    ok((await lead(A.id)).s === 'NEGOTIATION', 'el lead quedó en NEGOTIATION')
    r = await S('POST', `/api/proyectos/${A.sol}/fases/retroceder`, { fase: 'demo' })
    ok(r.status === 400, 'retroceder exige motivo')
    r = await S('POST', `/api/proyectos/${A.sol}/fases/retroceder`, { fase: 'demo', motivo: 'faltó validar con el cliente' })
    ok(r.status === 200 && r.j.faseActual === 'demo' && (await lead(A.id)).s === 'DEMO_VALIDATION', 'retroceder a demo (y el lead lo sigue)', r)
    g = await fases(A.sol)
    ok(g.fases[3].puerta.criterios.every(c => !c.ok), 'los criterios de las fases repetidas quedan sin marcar')
    for (const esperada of ['propuesta', 'negociacion']) await S('POST', `/api/proyectos/${A.sol}/fases/avanzar`, { forzar: true })

    // PRD y un sprint de preventa para comprobar la conversión
    await q(`UPDATE "Solucion" SET prd = '# PRD de preventa' WHERE id = $1`, A.sol)
    let sprintCreado = false
    try {
      await q(`INSERT INTO "Sprint" (id, "sprintCode", "solucionId", name, status, "createdAt") VALUES ('parity-sprint-a', 'PARITY-S1', $1, 'PARITY-TEST sprint', 'PLANNED', NOW())`, A.sol)
      sprintCreado = true
    } catch (e) { console.log('  (no se pudo crear el sprint de prueba: ' + e.message.slice(0, 80) + ')') }

    r = await S('POST', `/api/proyectos/${A.sol}/fases/avanzar`, {})
    ok(r.status === 400, 'la puerta de negociación exige el resultado', r)
    r = await S('POST', `/api/proyectos/${A.sol}/fases/avanzar`, { resultado: 'GANADO' })
    ok(r.status === 400 && r.j.faltan?.length === 2, 'GANADO exige los criterios', r)
    await marcarTodos(A.sol, 'negociacion', 2)
    r = await S('POST', `/api/proyectos/${A.sol}/fases/avanzar`, { resultado: 'GANADO', nota: 'firmado' })
    ok(r.status === 200 && r.j.faseActual === 'arranque', 'GANADO: el proyecto pasa a arranque', r)
    const l = await lead(A.id)
    ok(l.s === 'RESULT' && l.outcome === 'WON' && l.a === 'Project', 'lead ganado y asociado a Project', l)
    const so = (await q(`SELECT nombre, tipo, prd FROM "Solucion" WHERE id = $1`, A.sol))[0]
    ok(so.nombre === 'PARITY-TEST Fases A — Project' && so.tipo === 'PROJECT' && so.prd === '# PRD de preventa', 'la misma solución, ahora proyecto, conserva su PRD', so)
    const vs = (await S('GET', `/api/proyectos/${A.sol}/prd/versiones`)).j
    ok(vs.length === 1 && vs[0].version === 1, 'versión 1 del PRD guardada', vs)
    const v1 = (await S('GET', `/api/proyectos/${A.sol}/prd/versiones/1`)).j
    ok(v1.contenido === '# PRD de preventa', 'se puede leer el contenido de la versión')
    if (sprintCreado) {
      const m = (await q(`SELECT metadata FROM "Sprint" WHERE id = 'parity-sprint-a'`))[0].metadata
      ok(m?.origen === 'PREVENTA', 'el sprint existente quedó marcado como PREVENTA', m)
    }
    ok((await q(`SELECT COUNT(*)::int n FROM "FaseActividad" WHERE "solucionId" = $1`, A.sol))[0].n === 19, '19 actividades creadas hasta arranque')
    g = await fases(A.sol)
    ok(g.ventaConfirmada === true, 'la vista marca la venta como confirmada')
    r = await S('POST', `/api/proyectos/${A.sol}/fases/retroceder`, { fase: 'demo', motivo: 'x' })
    ok(r.status === 400, 'con la venta confirmada no se vuelve a la preventa', r)
    r = await S('PUT', `/api/leads/${A.id}`, { status: 'NEW', solucionAsociada: 'Project' })
    g = await fases(A.sol)
    ok(g.faseActual === 'arranque', 'cambios del lead no mueven un proyecto que ya está en ejecución', g.faseActual)

    for (const esperada of ['diseno_plan', 'construccion', 'qa', 'despliegue', 'entrega_cierre']) {
      r = await S('POST', `/api/proyectos/${A.sol}/fases/avanzar`, { forzar: true })
      ok(r.status === 200 && r.j.faseActual === esperada, `forzar → ${esperada}`, r)
    }
    r = await S('POST', `/api/proyectos/${A.sol}/fases/avanzar`, { forzar: true })
    g = await fases(A.sol)
    ok(r.status === 200 && g.estado === 'COMPLETADO' && g.fases.every(f => f.estado === 'HECHA'), 'la última fase cierra el proyecto (COMPLETADO)', g.estado)
    ok((await S('POST', `/api/proyectos/${A.sol}/fases/avanzar`, {})).status === 400, 'un proyecto completado no avanza más')

    console.log('— B: perdido desde el formulario del lead')
    const B = await nuevoLead('PARITY-TEST Fases B')
    r = await S('PUT', `/api/leads/${B.id}`, { status: 'RESULT', outcome: 'LOST', lostReason: 'PARITY-TEST precio', solucionAsociada: 'MVP' })
    g = await fases(B.sol)
    ok(r.status === 200 && g.estado === 'CERRADO_PERDIDO' && g.fases.find(f => f.estado === 'CERRADA'), 'LOST desde el lead cierra el proyecto', g.estado)
    ok((await S('POST', `/api/proyectos/${B.sol}/fases/avanzar`, {})).status === 400, 'un proyecto perdido no avanza')

    console.log('— C: ganado desde el formulario del lead')
    const C = await nuevoLead('PARITY-TEST Fases C')
    r = await S('PUT', `/api/leads/${C.id}`, { status: 'RESULT', outcome: 'WON', solucionAsociada: 'MVP' })
    g = await fases(C.sol)
    ok(r.status === 200 && g.faseActual === 'arranque' && g.estado === 'EN_CURSO', 'WON desde el lead lleva el proyecto a arranque', g.faseActual)

    console.log('— D: perdido desde la puerta')
    const D = await nuevoLead('PARITY-TEST Fases D')
    for (let i = 0; i < 5; i++) await S('POST', `/api/proyectos/${D.sol}/fases/avanzar`, { forzar: true })
    ok((await fases(D.sol)).faseActual === 'negociacion', 'D llegó a negociación')
    r = await S('POST', `/api/proyectos/${D.sol}/fases/avanzar`, { resultado: 'PERDIDO' })
    ok(r.status === 400, 'PERDIDO exige motivo')
    r = await S('POST', `/api/proyectos/${D.sol}/fases/avanzar`, { resultado: 'PERDIDO', motivo: 'PARITY-TEST se fue con otro' })
    const ld = await lead(D.id)
    ok(r.status === 200 && ld.s === 'RESULT' && ld.outcome === 'LOST' && ld.r === 'PARITY-TEST se fue con otro', 'PERDIDO desde la puerta cierra el lead con su motivo', ld)

    console.log('— F: lead anterior a «todo lead con motor» (sin proyecto) se inicia desde Oficina > Motor')
    const F = await nuevoLead('PARITY-TEST Fases F')
    await q(`DELETE FROM "BacklogItem" WHERE "solucionId" = $1`, F.sol)
    await q(`DELETE FROM "Solucion" WHERE id = $1`, F.sol)
    let res = (await S('GET', '/api/motor/resumen')).j
    ok(res.leadsSinMotor.some(x => x.leadId === F.id), 'el lead aparece en «Leads sin motor»', res.totales)
    ok((await llamar(cUser, 'POST', `/api/motor/leads/${F.id}/iniciar`, {})).status === 403, 'un usuario normal no puede iniciar el motor de un lead (403)')
    r = await S('POST', `/api/motor/leads/${F.id}/iniciar`, {})
    ok(r.status === 200 && !!r.j.solucionId, 'iniciar el lead crea su solución y arranca el motor', r)
    sols.push(r.j.solucionId)
    g = await fases(r.j.solucionId)
    ok(g.iniciado && g.faseActual === 'identificacion', 'queda en identificación', g.faseActual)
    ok((await q(`SELECT COUNT(*)::int n FROM "BacklogItem" WHERE "solucionId" = $1`, r.j.solucionId))[0].n === 2, 'con sus dos tareas')
    ok((await q(`SELECT nombre FROM "Solucion" WHERE id = $1`, r.j.solucionId))[0].nombre === 'PARITY-TEST Fases F — MVP', 'solución creada con el nombre del lead')
    res = (await S('GET', '/api/motor/resumen')).j
    ok(!res.leadsSinMotor.some(x => x.leadId === F.id) && res.cartera.some(x => x.leadId === F.id), 'sale de «sin motor» y entra a la cartera')
    r = await S('POST', `/api/motor/leads/${F.id}/iniciar`, {})
    ok(r.status === 200, 'iniciar otra vez no falla ni duplica', r)
    ok((await q(`SELECT COUNT(*)::int n FROM "BacklogItem" WHERE "solucionId" = $1`, r.j.solucionId))[0].n === 2, 'sigue habiendo solo dos tareas')

    console.log('— G: dónde está cada fase, vista previa e inicio en una fase elegida')
    g = await fases(A.sol)
    ok(g.fases.every(f => Array.isArray(f.recursos) && f.recursos.length > 0), 'todas las fases traen su «dónde está»')
    const rec1 = g.fases[0].recursos.find(r => r.tipo === 'LEAD_HUB')
    ok(rec1 && rec1.href === `/leads/${A.id}/hub`, 'identificación apunta al Lead Hub del lead', rec1)
    const recPrd = g.fases[6].recursos.find(r => r.tipo === 'HUB_PRD')
    ok(recPrd && recPrd.href === `/solutions/pilots/${A.sol}?seccion=prd` && recPrd.estado.startsWith('Aprobado') === false, 'arranque apunta a la pestaña PRD del hub', recPrd)
    const Gl = await nuevoLead('PARITY-TEST Fases G')
    await q(`DELETE FROM "BacklogItem" WHERE "solucionId" = $1`, Gl.sol)
    await q(`DELETE FROM "ProyectoFase" WHERE "solucionId" = $1`, Gl.sol)
    g = await fases(Gl.sol)
    ok(g.iniciado === false && g.fases.length === 12 && g.fases.every(f => f.estado === 'PENDIENTE') && g.fases[0].recursos.length > 0, 'sin iniciar: vista previa de las 12 fases', g.iniciado)
    r = await S('POST', `/api/proyectos/${Gl.sol}/fases/iniciar`, { fase: 'inventada' })
    ok(r.status === 400, 'una fase inexistente da 400', r)
    r = await S('POST', `/api/proyectos/${Gl.sol}/fases/iniciar`, { fase: 'construccion' })
    ok(r.status === 400, 'no se inicia en ejecución si la venta no está confirmada', r)
    r = await S('POST', `/api/proyectos/${Gl.sol}/fases/iniciar`, { fase: 'demo' })
    ok(r.status === 200 && r.j.faseActual === 'demo', 'inicia en la fase elegida (demo)', r)
    ok((await lead(Gl.id)).s === 'DEMO_VALIDATION', 'y el lead queda en Demo')
    ok((await q(`SELECT COUNT(*)::int n FROM "BacklogItem" WHERE "solucionId" = $1`, Gl.sol))[0].n === 3, 'con las tres tareas de Demo')

    console.log('— E: solución sin lead arranca en la fase de arranque')
    const eid = 'parity-sol-e'
    sols.push(eid)
    await q(`INSERT INTO "Solucion" (id, nombre, tipo, estado, "updatedAt") VALUES ($1, 'PARITY-TEST Fases E', 'PROJECT', 'ACTIVO', NOW())`, eid)
    r = await S('POST', `/api/proyectos/${eid}/fases/iniciar`, {})
    ok(r.status === 200 && r.j.faseActual === 'arranque', 'sin lead: empieza en arranque', r)
  } catch (e) {
    fallos++
    console.log(' FALLA excepción:', e)
  } finally {
    const ids = sols.slice()
    await q(`DELETE FROM "Notification" WHERE link ~ $1 OR message LIKE '%PARITY-TEST%'`, ids.length ? `p=(${ids.join('|')})$` : '^$').catch(() => {})
    await q(`DELETE FROM "Sprint" WHERE id = 'parity-sprint-a'`).catch(() => {})
    await q(`DELETE FROM "BacklogItem" WHERE "solucionId" = ANY($1::text[])`, ids).catch(() => {})
    await q(`DELETE FROM "Activity" WHERE "leadId" = ANY($1::text[])`, leads).catch(() => {})
    await q(`DELETE FROM "Solucion" WHERE id = ANY($1::text[])`, ids).catch(() => {})
    await q(`DELETE FROM "Lead" WHERE id = ANY($1::text[])`, leads).catch(() => {})
    await q(`DELETE FROM "Cliente" WHERE nombre LIKE 'PARITY-TEST Fases%'`).catch(() => {})
    const resto = (await q(`SELECT (SELECT COUNT(*) FROM "Solucion" WHERE nombre LIKE 'PARITY-TEST%')::int s, (SELECT COUNT(*) FROM "Lead" WHERE "companyName" LIKE 'PARITY-TEST%')::int l, (SELECT COUNT(*) FROM "ProyectoFase" WHERE "solucionId" = ANY($1::text[]))::int f, (SELECT COUNT(*) FROM "Notification" WHERE message LIKE '%PARITY-TEST%')::int n`, ids))[0]
    console.log('restos tras limpiar:', JSON.stringify(resto))
    await prisma.$disconnect()
    console.log(fallos === 0 ? '\nTODO OK' : `\n${fallos} FALLA(S)`)
    process.exit(fallos === 0 ? 0 : 1)
  }
})()
