// Notas del Lead Hub que tomaría una persona del equipo comercial (la reunión de diagnóstico es simulada).
const fs = require('fs')
const path = require('path')
const ROOT = '/root/portal-architechia'
const { PrismaClient } = require(path.join(ROOT, 'node_modules/@prisma/client'))
const prisma = new PrismaClient()
const IDS = JSON.parse(fs.readFileSync('/root/crm_ids.json', 'utf8'))
const tabs = (arr) => JSON.stringify({ tabs: arr.map((t, i) => ({ id: 't' + i, name: t[0], content: t[1] })) })
const NOTAS = {
  CONTACTED: tabs([['Primer contacto', '<p>Llamada con Marta Quintero (jefa comercial). Interés alto: hoy pierden cotizaciones por falta de seguimiento. Quedó agendada la reunión de diagnóstico para el jueves a las 10:00 por Meet, con ella y dos vendedores.</p>']]),
  DIAGNOSIS: tabs([
    ['Contexto del cliente', '<p>Distribuidora Andina SAS distribuye insumos de ferretería en el Eje Cafetero. Equipo comercial: 8 vendedores en calle, 1 jefa comercial y 1 asistente. Unos 420 clientes activos; ventas cercanas a COP 2.800 millones al año.</p>'],
    ['Dolores', '<ul><li>El seguimiento de cada cliente vive en Excel y en el WhatsApp personal de cada vendedor.</li><li>Cotizaciones que se quedan sin respuesta porque nadie recuerda hacer el seguimiento.</li><li>La jefa comercial no ve el pipeline: arma un reporte manual cada mes.</li><li>Cuando un vendedor se va, se pierde el historial de sus clientes.</li></ul>'],
    ['Requisitos iniciales', '<ul><li><b>Empresas</b>: NIT, razón social, ciudad, sector, vendedor asignado.</li><li><b>Contactos</b>: nombre, cargo, teléfono, email, empresa.</li><li><b>Oportunidades</b>: título, empresa, valor, etapa (Prospecto, Contactado, Cotizado, Negociación, Ganada, Perdida), fecha estimada de cierre, vendedor. Vista de tablero tipo kanban para mover oportunidades de etapa.</li><li><b>Actividades</b>: llamada, visita, correo o tarea; fecha, recordatorio y estado (pendiente, hecha). Alerta de actividades vencidas.</li><li><b>Tablero de KPIs</b>: oportunidades abiertas, valor del pipeline, tasa de cierre, actividades vencidas.</li><li><b>Roles</b>: el vendedor ve solo lo suyo; la jefa comercial ve todo.</li></ul>'],
    ['Restricciones y fuera de alcance', '<p>Presupuesto de COP 18.000.000 y entrega en 6 semanas. Los datos actuales están en Excel y se importarán después del MVP. Fuera del MVP: integración con WhatsApp, facturación electrónica y app móvil.</p>'],
  ]),
}
;(async () => {
  for (const [phase, content] of Object.entries(NOTAS)) {
    const ya = await prisma.$queryRawUnsafe(`SELECT id FROM "LeadHub" WHERE "leadId" = $1 AND phase = $2`, IDS.leadId, phase)
    if (ya.length) await prisma.$executeRawUnsafe(`UPDATE "LeadHub" SET content = $2, "updatedAt" = NOW() WHERE id = $1`, ya[0].id, content)
    else await prisma.$executeRawUnsafe(`INSERT INTO "LeadHub" (id, "leadId", phase, content, "updatedBy", "createdAt", "updatedAt") VALUES (gen_random_uuid()::text, $1, $2, $3, 'prueba-crm', NOW(), NOW())`, IDS.leadId, phase, content)
    console.log('nota guardada en', phase)
  }
  await prisma.$disconnect()
})()
