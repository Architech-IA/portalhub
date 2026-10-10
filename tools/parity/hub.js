#!/usr/bin/env node
// Prueba de integración del hub de la solución (MASD-0025) contra el servicio real (Rust :3100) y la base real.
// Crea soluciones PARITY-TEST, recorre guardado seguro, aprobación, backlog desde el PRD, requisitos que siguen a su tarea,
// hitos con dinero y aceptación, cambios, economía, coherencia, fotografías, enlace del cliente y adicionales, y LIMPIA todo.
// Con --ia también prueba la generación con IA (PRD en dos llamadas y diagrama Mermaid).
const path = require('path')
const ROOT = '/root/portal-architechia'
const { encode } = require(path.join(ROOT, 'node_modules/next-auth/jwt'))
const { PrismaClient } = require(path.join(ROOT, 'node_modules/@prisma/client'))
const prisma = new PrismaClient()
const BASE = 'http://127.0.0.1:3100'
const CON_IA = process.argv.includes('--ia')

let fallos = 0
const ok = (cond, msg, extra) => { if (!cond) fallos++; console.log(`${cond ? '  ok  ' : ' FALLA'} ${msg}${!cond && extra !== undefined ? ' → ' + JSON.stringify(extra).slice(0, 400) : ''}`) }
const q = (sql, ...p) => prisma.$queryRawUnsafe(sql, ...p)
const id = () => 'pt' + Math.random().toString(36).slice(2, 12)

;(async () => {
  const u = (await q(`SELECT id, name, email FROM "User" WHERE role='SUPERADMIN' ORDER BY ctid LIMIT 1`))[0]
  const cookieDe = async (role) => {
    const t = await encode({ token: { name: u.name, email: u.email, sub: u.id, id: u.id, role }, secret: process.env.NEXTAUTH_SECRET, maxAge: 900 })
    return `__Secure-next-auth.session-token=${t}; next-auth.session-token=${t}`
  }
  const cAdmin = await cookieDe('ADMIN'), cUser = await cookieDe('USER')
  const llamar = async (cookie, metodo, ruta, cuerpo) => {
    const r = await fetch(BASE + ruta, { method: metodo, headers: { 'content-type': 'application/json', ...(cookie ? { cookie } : {}) }, body: cuerpo === undefined ? undefined : JSON.stringify(cuerpo) })
    return { status: r.status, j: await r.json().catch(() => ({})) }
  }
  const A = (m, r, b) => llamar(cAdmin, m, r, b)
  const U = (m, r, b) => llamar(cUser, m, r, b)
  const sols = []
  const nueva = async (extra = {}) => {
    const sid = id(); sols.push(sid)
    await q(`INSERT INTO "Solucion" (id, nombre, tipo, estado, "valorEstimado", descripcion, prd, "disenoTecnico", "createdAt", "updatedAt") VALUES ($1, $2, 'PROJECT', $3, $4, $5, $6, $7, NOW(), NOW())`,
      sid, extra.nombre ?? 'PARITY-TEST Hub', extra.estado ?? 'PAUSADO', extra.valor ?? 5000, extra.descripcion ?? 'Descripción de prueba', extra.prd ?? '{}', extra.diseno ?? '{}')
    return sid
  }
  const col = async (sid, c) => (await q(`SELECT "${c}" v FROM "Solucion" WHERE id = $1`, sid))[0].v
  const prdBase = (extra = {}) => JSON.stringify({
    estadoDocumento: 'BORRADOR', resumenEjecutivo: '<p>Resumen</p>', requisitos: [
      { id: 'r1', tipo: 'historia', texto: '<p>El vendedor registra una oportunidad</p>', criterioAceptacion: '<p>Queda guardada con cliente y valor</p>', prioridad: 'MUST', estado: 'PROPUESTO' },
      { id: 'r2', tipo: 'historia', texto: 'Reporte mensual', criterioAceptacion: 'Se descarga en PDF', prioridad: 'SHOULD', estado: 'PROPUESTO' },
    ], ...extra,
  })
  let r

  try {
    console.log('— A: guardado seguro')
    const s1 = await nueva({ prd: prdBase() })
    const rawPrd = await col(s1, 'prd')
    r = await A('PUT', `/api/soluciones/${s1}`, { prd: rawPrd, base: { prd: rawPrd } })
    const fila = (await q(`SELECT descripcion, estado, "valorEstimado" v FROM "Solucion" WHERE id = $1`, s1))[0]
    ok(r.status === 200 && fila.descripcion === 'Descripción de prueba' && fila.estado === 'PAUSADO' && fila.v === 5000, 'guardar solo el PRD no reinicia descripción, estado ni valor', fila)
    // otro proceso cambia el PRD
    const otro = JSON.parse(rawPrd); otro.resumenEjecutivo = '<p>Cambiado por otra persona</p>'
    await q(`UPDATE "Solucion" SET prd = $2 WHERE id = $1`, s1, JSON.stringify(otro))
    const mio = JSON.parse(rawPrd); mio.problema = '<p>Mi edición</p>'
    r = await A('PUT', `/api/soluciones/${s1}`, { prd: JSON.stringify(mio), base: { prd: rawPrd } })
    ok(r.status === 409 && r.j.conflictos?.includes('prd') && typeof r.j.vigente?.prd === 'string', 'edición simultánea: 409 con el contenido vigente', r)
    ok(JSON.parse(await col(s1, 'prd')).problema === undefined, 'el conflicto no escribió nada')
    r = await A('PUT', `/api/soluciones/${s1}`, { prd: JSON.stringify(mio), base: { prd: rawPrd }, forzar: true })
    ok(r.status === 200 && JSON.parse(await col(s1, 'prd')).problema === '<p>Mi edición</p>', 'con forzar se sobrescribe')
    ok((await q(`SELECT COUNT(*)::int n FROM "SolucionPrdVersion" WHERE "solucionId" = $1`, s1))[0].n >= 1, 'el PRD reemplazado queda como versión')

    // el servidor cambia solo el avance de un requisito: no es conflicto y no se pierde
    const actual = JSON.parse(await col(s1, 'prd')); actual.requisitos[0].estado = 'VERIFICADO'; actual.requisitos[0].backlogItemId = 'tarea-x'
    const baseTxt = JSON.stringify(JSON.parse(await col(s1, 'prd')))
    await q(`UPDATE "Solucion" SET prd = $2 WHERE id = $1`, s1, JSON.stringify(actual))
    const mio2 = JSON.parse(baseTxt); mio2.objetivoGeneral = '<p>Objetivo nuevo</p>'
    r = await A('PUT', `/api/soluciones/${s1}`, { prd: JSON.stringify(mio2), base: { prd: baseTxt } })
    const tras = JSON.parse(await col(s1, 'prd'))
    ok(r.status === 200 && tras.requisitos[0].estado === 'VERIFICADO' && tras.requisitos[0].backlogItemId === 'tarea-x' && tras.objetivoGeneral === '<p>Objetivo nuevo</p>', 'guardar no deshace el avance que el servidor registró en los requisitos', tras.requisitos[0])
    const otroLead = (await q(`SELECT "leadId" l FROM "Solucion" WHERE "leadId" IS NOT NULL LIMIT 1`))[0]?.l
    if (otroLead) { r = await A('PUT', `/api/soluciones/${s1}`, { leadId: otroLead }); ok(r.status === 409, 'un lead que ya tiene Solución da 409 claro', r); ok((await col(s1, 'leadId')) === null, 'y no se movió el lead') }

    console.log('— B: aprobación y reapertura')
    let prd = JSON.parse(await col(s1, 'prd')); prd.estadoDocumento = 'APROBADO'
    r = await U('PUT', `/api/soluciones/${s1}`, { prd: JSON.stringify(prd) })
    ok(r.status === 403, 'un usuario normal no puede aprobar (403)', r)
    r = await A('PUT', `/api/soluciones/${s1}`, { prd: JSON.stringify(prd) })
    let guardado = JSON.parse(await col(s1, 'prd'))
    ok(r.status === 200 && guardado.estadoDocumento === 'APROBADO' && guardado.aprobacion?.porNombre && guardado.aprobacion?.en, 'un administrador aprueba y queda quién y cuándo', guardado.aprobacion)
    guardado.problema = '<p>Cambié algo después de aprobar</p>'
    r = await A('PUT', `/api/soluciones/${s1}`, { prd: JSON.stringify(guardado) })
    let reab = JSON.parse(await col(s1, 'prd'))
    ok(reab.estadoDocumento === 'EN_REVISION' && !reab.aprobacion && reab.reabierto, 'editar un documento aprobado lo devuelve a revisión', { e: reab.estadoDocumento })

    console.log('— C: línea base, backlog desde el PRD y requisitos que siguen a su tarea')
    r = await A('POST', `/api/soluciones/${s1}/backlog-desde-prd`)
    ok(r.status === 400, 'sin PRD y diseño aprobados no se genera backlog (400)', r)
    const dis = { estadoDocumento: 'APROBADO', entidades: [{ id: 'e1', nombre: 'Oportunidad', atributos: 'valor, cliente', relaciones: '' }], stack: [{ id: 's1', capa: 'Backend', tecnologia: 'Rust', justificacion: 'x' }] }
    prd = JSON.parse(await col(s1, 'prd')); prd.estadoDocumento = 'APROBADO'
    prd.requisitos.forEach(x => { delete x.backlogItemId; x.estado = 'PROPUESTO' })
    await q(`UPDATE "Solucion" SET prd = $2 WHERE id = $1`, s1, JSON.stringify({ ...prd, estadoDocumento: 'EN_REVISION' }))
    r = await A('PUT', `/api/soluciones/${s1}`, { prd: JSON.stringify(prd), disenoTecnico: JSON.stringify(dis) })
    ok(r.status === 200 && (await q(`SELECT COUNT(*)::int n FROM "SolucionSnapshot" WHERE "solucionId" = $1 AND tipo = 'LINEA_BASE'`, s1))[0].n === 1, 'PRD y diseño aprobados crean la línea base', r)
    r = await A('PUT', `/api/soluciones/${s1}`, { descripcion: 'otra' })
    ok((await q(`SELECT COUNT(*)::int n FROM "SolucionSnapshot" WHERE "solucionId" = $1 AND tipo = 'LINEA_BASE'`, s1))[0].n === 1, 'la línea base no se duplica')
    const [c1, c2] = await Promise.all([A('POST', `/api/soluciones/${s1}/backlog-desde-prd`), A('POST', `/api/soluciones/${s1}/backlog-desde-prd`)])
    const total = (c1.j.creadas ?? 0) + (c2.j.creadas ?? 0)
    ok(c1.status === 200 && c2.status === 200 && total === 2, 'dos pedidos a la vez crean las tareas una sola vez', [c1.j, c2.j])
    const items = await q(`SELECT id, status, "prdRequisitoId" rid, title, priority FROM "BacklogItem" WHERE "solucionId" = $1 ORDER BY "createdAt"`, s1)
    const enPrd = JSON.parse(await col(s1, 'prd'))
    ok(items.length === 2 && enPrd.requisitos.every(x => items.some(i => i.id === x.backlogItemId && i.rid === x.id)), 'cada requisito quedó ligado a su tarea en el PRD', enPrd.requisitos.map(x => x.backlogItemId))
    ok(items.find(i => i.rid === 'r1')?.priority === 'HIGH' && items.find(i => i.rid === 'r2')?.priority === 'MEDIUM', 'MUST→HIGH y SHOULD→MEDIUM', items.map(i => i.priority))
    r = await A('GET', `/api/backlog?ligero=1&solucionId=${s1}&conRequisito=1`)
    ok(r.status === 200 && Array.isArray(r.j) && r.j.length === 2, 'el backlog se pide solo de esta solución', r.j?.length)
    const t1 = items.find(i => i.rid === 'r1')
    await A('PUT', `/api/backlog/${t1.id}`, { status: 'DONE' })
    ok(JSON.parse(await col(s1, 'prd')).requisitos[0].estado === 'IMPLEMENTADO', 'tarea hecha → requisito IMPLEMENTADO')
    await A('PUT', `/api/backlog/${t1.id}`, { status: 'FAILED' })
    ok(JSON.parse(await col(s1, 'prd')).requisitos[0].estado === 'APROBADO', 'tarea fallida → el requisito vuelve a APROBADO')

    console.log('— D: hitos, dinero y aceptación')
    r = await A('POST', '/api/hitos', { solucionId: s1, titulo: 'Entrega 1', monto: 2000, fechaComprometida: '2020-01-01' })
    const h1 = r.j
    ok(r.status === 200 && h1.monto === 2000 && h1.estadoPago === 'PENDIENTE', 'hito con monto', r.j)
    ok((await U('PUT', `/api/hitos/${h1.id}`, { estadoPago: 'PAGADO' })).status === 403, 'un usuario normal no marca pagos (403)')
    ok((await A('PUT', `/api/hitos/${h1.id}`, { estadoPago: 'FACTURADO' })).status === 200, 'un administrador marca la factura')
    ok((await U('PUT', `/api/hitos/${h1.id}`, { aceptar: { nombre: 'Cliente' } })).status === 403, 'un usuario normal no registra la aceptación (403)')
    r = await A('PUT', `/api/hitos/${h1.id}`, { estado: 'CUMPLIDO', aceptar: { nombre: 'Ana Pérez (gerente)', nota: 'Correo del 5/10' } })
    ok(r.status === 200 && r.j.aceptadoPor === 'Ana Pérez (gerente)' && r.j.aceptadoEn && r.j.fechaReal, 'aceptación del cliente con fecha real automática', r.j)
    const sp = id(); await q(`INSERT INTO "Sprint" (id, name, "solucionId", status, "createdAt") VALUES ($1, 'Sprint de prueba', $2, 'ACTIVE', NOW())`, sp, s1)
    await q(`UPDATE "BacklogItem" SET "sprintId" = $2 WHERE id = $1`, t1.id, sp)
    await A('PUT', `/api/hitos/${h1.id}`, { sprintId: sp })
    r = await A('GET', `/api/hitos?solucionId=${s1}`)
    ok(r.j[0]?.sprint?.total === 1 && r.j[0].sprint.hechas === 0, 'el hito muestra el avance de su sprint', r.j[0]?.sprint)
    r = await A('PUT', `/api/hitos/${h1.id}`, { aceptar: null })
    ok(r.status === 200 && r.j.aceptadoEn === null, 'quitar la aceptación')

    console.log('— E: control de cambios y economía')
    r = await U('POST', `/api/soluciones/${s1}/cambios`, { titulo: 'Agregar módulo de reportes', impactoCosto: 800, impactoDias: 5, impactoAlcance: 'Nuevo módulo' })
    const cc = r.j
    ok(r.status === 200 && cc.estado === 'SOLICITADA', 'cualquier usuario registra una solicitud', r)
    ok((await U('PUT', `/api/cambios/${cc.id}`, { estado: 'APROBADA' })).status === 403, 'un usuario normal no aprueba (403)')
    r = await A('PUT', `/api/cambios/${cc.id}`, { estado: 'APROBADA', decisionNota: 'OK con el cliente' })
    ok(r.status === 200 && r.j.aplicado === true && r.j.decididoPor, 'aprobar queda con quién decidió', r.j)
    ok((await col(s1, 'valorEstimado')) === 5800, 'aprobar suma el costo al valor del proyecto', await col(s1, 'valorEstimado'))
    ok((await A('DELETE', `/api/cambios/${cc.id}`)).status === 400, 'un cambio ya sumado no se borra directamente')
    r = await A('PUT', `/api/cambios/${cc.id}`, { estado: 'RECHAZADA' })
    ok(r.status === 200 && (await col(s1, 'valorEstimado')) === 5000, 'rechazarlo revierte el valor')
    r = await A('GET', `/api/soluciones/${s1}/economia`)
    ok(r.status === 200 && r.j.valorEstimado === 5000 && r.j.hitos.total === 1 && r.j.tareas.total === 2 && r.j.tokens, 'economía', r.j)

    console.log('— F: coherencia y propuesta')
    r = await A('GET', `/api/soluciones/${s1}/coherencia`)
    ok(r.status === 200 && Array.isArray(r.j.hallazgos) && r.j.hallazgos.some(h => h.area === 'Cumplimiento'), 'la revisión devuelve hallazgos (hito vencido, pagos que no cuadran…)', r.j)
    ok(r.j.hallazgos.some(h => /no cuadra/.test(h.mensaje)), 'detecta que el calendario de pagos no cuadra con el valor')
    ok((await A('POST', `/api/soluciones/${s1}/cruce-propuesta`)).status === 400, 'sin propuestas el cruce da 400')

    console.log('— G: fotografías, enlace del cliente y adicionales')
    ok((await U('POST', `/api/soluciones/${s1}/snapshots`, { tipo: 'AS_BUILT' })).status === 403, 'solo un administrador congela el as-built')
    r = await A('POST', `/api/soluciones/${s1}/snapshots`, { tipo: 'AS_BUILT', etiqueta: 'Entrega' })
    ok(r.status === 200, 'as-built', r)
    r = await A('GET', `/api/soluciones/${s1}/snapshots`)
    ok(r.j.length === 2, 'lista de fotografías', r.j)
    r = await A('GET', `/api/soluciones/${s1}/snapshots/${r.j[0].id}`)
    ok(r.status === 200 && r.j.contenido?.prd && r.j.contenido.hitos?.length === 1, 'la fotografía trae los documentos y los hitos')
    ok((await U('POST', `/api/soluciones/${s1}/enlace-cliente`)).status === 403, 'solo un administrador crea el enlace')
    r = await A('POST', `/api/soluciones/${s1}/enlace-cliente`)
    const tok = r.j.token
    ok(r.status === 200 && /^[0-9a-f]{32}$/.test(tok), 'enlace creado', r.j)
    r = await llamar(null, 'GET', `/api/publico/proyecto/${tok}`)
    ok(r.status === 200 && r.j.nombre === 'PARITY-TEST Hub' && r.j.hitos?.length === 1, 'el cliente ve el proyecto sin iniciar sesión', r)
    const txt = JSON.stringify(r.j)
    ok(!/monto|valorEstimado|estadoPago|prd|riesgo|repositorio/i.test(txt), 'el enlace no filtra dinero, PRD, riesgos ni código', txt.slice(0, 200))
    ok((await llamar(null, 'GET', '/api/publico/proyecto/zzz')).status === 404, 'token inválido → 404')
    await A('DELETE', `/api/soluciones/${s1}/enlace-cliente`)
    ok((await llamar(null, 'GET', `/api/publico/proyecto/${tok}`)).status === 404, 'revocar corta el acceso')
    r = await U('POST', `/api/soluciones/${s1}/adicional`, { nombre: 'PARITY-TEST Fase 2', valorEstimado: 1000 })
    const hija = r.j; if (hija?.id) sols.push(hija.id)
    ok(r.status === 200 && hija.parentId === s1 && hija.leadId === null && hija.tipo === 'PROJECT', 'solución adicional ligada al original', r)
    r = await U('POST', `/api/soluciones/${hija.id}/adicional`, { nombre: 'PARITY-TEST Fase 3' })
    if (r.j?.id) sols.push(r.j.id)
    ok(r.j.parentId === s1, 'un adicional de un adicional cuelga del original', r.j.parentId)

    console.log('— H: diagramas en el guardado y borrado protegido')
    const dg = JSON.stringify([{ id: 'd1', titulo: 'ER', tipo: 'er', codigo: 'erDiagram\n  A ||--o{ B : tiene' }])
    r = await A('PUT', `/api/soluciones/${s1}`, { diagramas: dg, base: { diagramas: await col(s1, 'diagramas') } })
    ok(r.status === 200 && (await col(s1, 'diagramas')) === dg, 'los diagramas se guardan')
    r = await A('PUT', `/api/soluciones/${s1}`, { prd: '[1]' })
    ok(r.status === 400, 'un PRD que no es un documento da 400', r)
    ok((await U('DELETE', `/api/soluciones/${s1}`)).status === 403, 'un usuario normal no elimina una solución')
    r = await A('DELETE', `/api/soluciones/${s1}`)
    ok(r.status === 409 && r.j.cuentas?.backlog === 2, 'con backlog pide confirmar (409)', r.j)
    r = await A('DELETE', `/api/soluciones/${s1}?forzar=1`)
    ok(r.status === 200, 'con forzar la elimina')

    if (CON_IA) {
      console.log('— I: IA (PRD en dos llamadas y diagrama)')
      const s2 = await nueva({ nombre: 'PARITY-TEST IA', descripcion: 'CRM a la medida para una constructora: oportunidades, cotizaciones, seguimiento y reportes. Integra WhatsApp.', diseno: JSON.stringify({ entidades: [{ id: 'e', nombre: 'Oportunidad', atributos: 'id, valor, etapa', relaciones: 'Cliente 1-N' }, { id: 'f', nombre: 'Cliente', atributos: 'id, nombre', relaciones: '' }] }) })
      const t0 = Date.now()
      r = await A('POST', `/api/soluciones/${s2}/prd-generate`)
      ok(r.status === 200 && r.j.prd?.requisitos?.length >= 10, `PRD con muchos requisitos (${r.j.prd?.requisitos?.length}) en ${Math.round((Date.now() - t0) / 1000)} s`, r.j.avisos ?? r)
      ok((r.j.prd?.requisitos ?? []).every(x => ['propuesta', 'preventa', 'inferido'].includes(x.fuente)), 'cada requisito dice de dónde sale (fuente)', (r.j.prd?.requisitos ?? []).map(x => x.fuente))
      ok(!('avisos' in r.j) || r.j.avisos.length === 0, 'sin avisos', r.j.avisos)
      r = await A('POST', `/api/soluciones/${s2}/diagramas-generate`, { tipo: 'er' })
      ok(r.status === 200 && /^erDiagram/.test(r.j.codigo ?? ''), 'diagrama ER en Mermaid', r.j)
    }
  } finally {
    for (const sid of sols) {
      await q(`DELETE FROM "BacklogItem" WHERE "solucionId" = $1`, sid).catch(() => undefined)
      await q(`DELETE FROM "Sprint" WHERE "solucionId" = $1`, sid).catch(() => undefined)
      await q(`DELETE FROM "Solucion" WHERE id = $1`, sid).catch(() => undefined)
    }
    await q(`DELETE FROM "Activity" WHERE description LIKE '%PARITY-TEST%'`).catch(() => undefined)
    const quedan = (await q(`SELECT COUNT(*)::int n FROM "Solucion" WHERE nombre LIKE 'PARITY-TEST%'`))[0].n
    console.log(`limpieza: quedan ${quedan} soluciones de prueba`)
    await prisma.$disconnect()
  }
  console.log(fallos === 0 ? '\nTODO OK' : `\n${fallos} FALLA(S)`)
  process.exit(fallos === 0 ? 0 : 1)
})().catch(e => { console.error('ERROR', e); process.exit(1) })
