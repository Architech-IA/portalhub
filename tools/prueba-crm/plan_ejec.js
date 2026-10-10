// Plan de ejecución del proyecto (lo que una persona deja en la pestaña «Plan de Ejecución» del hub).
const fs = require('fs')
const path = require('path')
const ROOT = '/root/portal-architechia'
const { PrismaClient } = require(path.join(ROOT, 'node_modules/@prisma/client'))
const prisma = new PrismaClient()
const IDS = JSON.parse(fs.readFileSync('/root/crm_ids.json', 'utf8'))
const plan = {
  estadoDocumento: 'APROBADO',
  qa: 'Cada tarea de código pasa tsc y su revisión semántica antes de integrarse al sprint; las pruebas unitarias (npm test) deben pasar. Antes de cada despliegue una persona corre la prueba de aceptación en navegador (Playwright) sobre el flujo comercial completo: empresas, contactos, oportunidades por etapas, actividades vencidas, KPIs, login y alcance por rol, y revisa en 390 px de ancho. El cliente acepta con un recorrido guiado.',
  ambientes: [
    { id: 'a1', ambiente: 'Demo / producción', proposito: 'Un solo ambiente con su base de datos dedicada (el despliegue del Motor no separa ambientes todavía)', despliega: 'Motor Agéntico: construye la imagen desde main y la publica detrás de Nginx con HTTPS', promocion: 'PR del sprint revisado y mergeado, migraciones aplicadas y prueba de aceptación en verde' },
  ],
  release: 'Despliegue desde main. Orden: mergear el PR del sprint, aplicar migraciones (prisma migrate deploy), definir SESSION_SECRET, desplegar, correr la prueba de aceptación. Plan de vuelta atrás: el despliegue conserva el contenedor anterior hasta que el nuevo responde; revertir el merge y redesplegar.',
  raci: [
    { id: 'r1', actividad: 'Aprobar el alcance y los cambios', responsable: 'Marta Quintero (cliente)', aprueba: 'Dirección ArchitechIA', consultado: 'Líder técnico', informado: 'Equipo' },
    { id: 'r2', actividad: 'Construir el producto (sprints)', responsable: 'Agentes del Motor', aprueba: 'Líder técnico', consultado: 'Cliente', informado: 'Dirección' },
    { id: 'r3', actividad: 'Revisar y mergear el PR de cada sprint', responsable: 'Líder técnico', aprueba: 'Líder técnico', consultado: 'Cliente', informado: 'Dirección' },
    { id: 'r4', actividad: 'Aceptación (UAT) y capacitación', responsable: 'Líder de proyecto', aprueba: 'Marta Quintero (cliente)', consultado: 'Vendedores', informado: 'Dirección' },
  ],
  cambios: 'Todo cambio de alcance se registra como solicitud de cambio con su estimación y la aprueban el cliente y Dirección antes de entrar a un sprint. Lo no incluido en la propuesta (WhatsApp, facturación electrónica, app móvil) es fase 2.',
  comunicacion: [
    { id: 'c1', que: 'Avance del proyecto', audiencia: 'Marta Quintero', frecuencia: 'Semanal', canal: 'Correo + reunión de 30 min', responsable: 'Líder de proyecto' },
    { id: 'c2', que: 'Demo de cada hito', audiencia: 'Cliente y dos vendedores', frecuencia: 'Por hito', canal: 'Meet', responsable: 'Líder técnico' },
  ],
}
;(async () => {
  await prisma.$executeRawUnsafe(`UPDATE "Solucion" SET "planEjecucion" = $2, "updatedAt" = NOW() WHERE id = $1`, IDS.solucionId, JSON.stringify(plan))
  console.log('plan de ejecución guardado y aprobado')
  await prisma.$disconnect()
})()
