// Lo que una persona haría en el hub de la Solución antes de construir el MVP: PRD corto, diseño técnico, diagrama y backlog del sprint «MVP».
const fs = require('fs')
const path = require('path')
const ROOT = '/root/portal-architechia'
const { PrismaClient } = require(path.join(ROOT, 'node_modules/@prisma/client'))
const prisma = new PrismaClient()
const IDS = JSON.parse(fs.readFileSync('/root/crm_ids.json', 'utf8'))
const SOL = IDS.solucionId
const AREA_DEV = '947ca771-fe9e-4c3f-bfea-2ef2e27986c6'
const rid = (k) => 'r' + k + Math.random().toString(36).slice(2, 6)

const CONVENCIONES = 'Convenciones del proyecto (están en el Diseño técnico): nombres del dominio en español; páginas y acciones usan Prisma desde "@/lib/db"; toda página que lee la base lleva `export const dynamic = \'force-dynamic\'` (en el build no hay base de datos); formularios con Server Actions ("use server"); no cambies prisma/schema.prisma ni package.json. Cada archivo nuevo debe compilar con `npx tsc --noEmit`.'

// requisito del PRD → tarea del sprint
const R = [
  { k: 'val', texto: 'Como desarrollador del equipo quiero funciones de validación puras (NIT, email, valores, etapas) para que todas las pantallas validen igual y se puedan probar sin base de datos.',
    crit: 'Existe src/lib/validaciones.ts con funciones exportadas validarNit, validarEmail, validarValor y siguienteEtapa; existe tests/validaciones.test.ts con casos de cada una y `npm test` pasa.',
    titulo: 'Validaciones puras del CRM (NIT, email, valor, etapas) con pruebas',
    desc: 'Crea src/lib/validaciones.ts exportando: validarNit(nit: string): boolean (solo dígitos y un guion final opcional con dígito, 6 a 12 caracteres), validarEmail(email: string): boolean, validarValor(v: number): boolean (número finito >= 0), y siguienteEtapa(actual: EtapaOportunidad): EtapaOportunidad | null que devuelve la siguiente en el orden PROSPECTO, CONTACTADO, COTIZADO, NEGOCIACION, GANADA (null después de GANADA o si es PERDIDA). Define el tipo EtapaOportunidad como unión de strings en ese mismo archivo (sin importar de @prisma/client para que las pruebas no necesiten base). Crea tests/validaciones.test.ts con node:test y assert (import { test } from "node:test") cubriendo casos válidos e inválidos de cada función.' },
  { k: 'dat', texto: 'Como vendedor quiero que el sistema guarde y consulte empresas, contactos, oportunidades y actividades de forma consistente.',
    crit: 'Existe src/lib/crm.ts con funciones tipadas para crear/listar empresas, contactos, oportunidades y actividades, mover la etapa de una oportunidad, marcar una actividad como hecha y calcular KPIs; usa las validaciones.',
    titulo: 'Capa de datos del CRM (src/lib/crm.ts) sobre Prisma',
    desc: 'Crea src/lib/crm.ts con funciones async tipadas sobre el cliente de "@/lib/db": crearEmpresa(datos), listarEmpresas(), obtenerEmpresa(id) (con contactos y oportunidades), crearContacto(datos), crearOportunidad(datos), listarOportunidades() (incluye la empresa), moverEtapa(id, etapa) que solo permite pasar a la etapa dada por siguienteEtapa o a PERDIDA, crearActividad(datos), listarActividades() (incluye contacto y oportunidad, orden por fecha), marcarHecha(id), y kpis() que devuelve { oportunidadesAbiertas, valorPipeline, tasaCierre, actividadesVencidas }: abiertas = etapa distinta de GANADA y PERDIDA; valorPipeline = suma de valor de las abiertas; tasaCierre = ganadas / (ganadas + perdidas) en porcentaje (0 si no hay cerradas); vencidas = no hechas con fecha anterior a hoy. Cada función de creación valida con src/lib/validaciones.ts y lanza Error con un mensaje en español si algo no es válido.' },
  { k: 'emp', texto: 'Como vendedor quiero ver y crear empresas con NIT, razón social, ciudad, sector y vendedor asignado.',
    crit: 'La ruta /empresas lista las empresas y tiene un formulario de alta que valida el NIT (único) y muestra el error; /empresas/[id] muestra el detalle con sus contactos y oportunidades.',
    titulo: 'Pantallas de empresas: listado, alta y detalle',
    desc: 'Crea src/app/empresas/page.tsx (lista de empresas en tabla + formulario de alta con Server Action que usa crearEmpresa; si hay error mostrar el mensaje) y src/app/empresas/[id]/page.tsx (detalle: datos de la empresa, lista de contactos y de oportunidades). Enlaza cada fila de la lista con su detalle.' },
  { k: 'con', texto: 'Como vendedor quiero registrar los contactos de cada empresa con nombre, cargo, teléfono y email.',
    crit: 'En el detalle de una empresa hay un formulario para agregar un contacto (valida el email) y la lista de contactos se actualiza.',
    titulo: 'Alta de contactos dentro del detalle de la empresa',
    desc: 'En src/app/empresas/[id]/page.tsx agrega un formulario (Server Action con crearContacto) para registrar un contacto de esa empresa con nombre, cargo, teléfono y email, y muestra los errores de validación. Después de guardar, la página debe mostrar el contacto nuevo (usa revalidatePath de "next/cache").' },
  { k: 'opp', texto: 'Como vendedor quiero un tablero por etapas para mover mis oportunidades y ver cuánto vale cada una.',
    crit: 'La ruta /oportunidades muestra una columna por etapa con sus oportunidades (título, empresa, valor), permite crear una oportunidad y moverla a la siguiente etapa o marcarla como perdida.',
    titulo: 'Tablero kanban de oportunidades',
    desc: 'Crea src/app/oportunidades/page.tsx: seis columnas (PROSPECTO, CONTACTADO, COTIZADO, NEGOCIACION, GANADA, PERDIDA) con las tarjetas de cada etapa (título, empresa, valor formateado en pesos colombianos, vendedor). Cada tarjeta tiene un botón "Avanzar" (Server Action con moverEtapa a la siguiente etapa) y "Perdida". Incluye un formulario para crear una oportunidad eligiendo la empresa de una lista.' },
  { k: 'act', texto: 'Como vendedor quiero registrar llamadas, visitas, correos y tareas con fecha y recordatorio, y ver las vencidas.',
    crit: 'La ruta /actividades lista las actividades ordenadas por fecha, resalta las vencidas, permite crear una y marcarla como hecha.',
    titulo: 'Pantalla de actividades con recordatorios y vencidas',
    desc: 'Crea src/app/actividades/page.tsx: tabla de actividades (tipo, descripción, fecha, recordatorio, estado) ordenada por fecha; las no hechas con fecha pasada se resaltan en rojo y dicen "Vencida"; botón "Marcar hecha" por fila (Server Action con marcarHecha); formulario de alta con tipo, descripción, fecha, recordatorio opcional y oportunidad opcional.' },
  { k: 'kpi', texto: 'Como jefa comercial quiero un tablero con los KPIs del equipo: oportunidades abiertas, valor del pipeline, tasa de cierre y actividades vencidas.',
    crit: 'La página de inicio muestra cuatro tarjetas con los KPIs calculados por kpis(), con formato legible.',
    titulo: 'Tablero de KPIs en la página de inicio',
    desc: 'Reemplaza src/app/page.tsx por un tablero con cuatro tarjetas (oportunidades abiertas, valor del pipeline en pesos colombianos, tasa de cierre en %, actividades vencidas) usando kpis() de src/lib/crm.ts. La página lleva `export const dynamic = \'force-dynamic\'`.' },
  { k: 'nav', texto: 'Como usuario quiero un menú para navegar entre inicio, empresas, oportunidades y actividades.',
    crit: 'El layout muestra una barra de navegación con enlaces a Inicio, Empresas, Oportunidades y Actividades en todas las páginas.',
    titulo: 'Navegación y estilo base en el layout',
    desc: 'Actualiza src/app/layout.tsx para incluir una barra de navegación superior (componente src/components/Nav.tsx) con enlaces a Inicio, Empresas, Oportunidades y Actividades, y un contenedor central de ancho máximo 1100px con márgenes. Usa estilos en línea o un archivo src/app/globals.css importado en el layout; sin librerías nuevas.' },
]

const DISENO = {
  estadoDocumento: 'EN_REVISION',
  arquitectura: 'Aplicación Next.js 14 (App Router) con Server Components y Server Actions que usan Prisma directamente contra PostgreSQL. Un solo despliegue (contenedor) y una base de datos dedicada al proyecto.',
  entidades: [
    { id: 'e1', nombre: 'Empresa', atributos: 'id, nit (único), razonSocial, ciudad, sector, vendedor, creadaEn', relaciones: 'tiene muchos Contacto; tiene muchas Oportunidad' },
    { id: 'e2', nombre: 'Contacto', atributos: 'id, nombre, cargo, telefono, email, empresaId, creadoEn', relaciones: 'pertenece a Empresa; tiene muchas Actividad' },
    { id: 'e3', nombre: 'Oportunidad', atributos: 'id, titulo, valor, etapa (PROSPECTO, CONTACTADO, COTIZADO, NEGOCIACION, GANADA, PERDIDA), cierreEstimado, vendedor, empresaId, creadaEn', relaciones: 'pertenece a Empresa; tiene muchas Actividad' },
    { id: 'e4', nombre: 'Actividad', atributos: 'id, tipo (LLAMADA, VISITA, CORREO, TAREA), descripcion, fecha, recordatorio, hecha, oportunidadId, contactoId, creadaEn', relaciones: 'pertenece a Oportunidad (opcional) y a Contacto (opcional)' },
    { id: 'e5', nombre: 'Usuario', atributos: 'id, nombre, email, rol (VENDEDOR, JEFATURA)', relaciones: 'posee Empresa, Oportunidad y Actividad por vendedor (llega con los roles, fase de endurecimiento)' },
  ],
  stack: [
    { id: 's1', capa: 'Aplicación', tecnologia: 'Next.js 14 (App Router) + TypeScript estricto', justificacion: 'el despliegue del Motor ya soporta esta plantilla' },
    { id: 's2', capa: 'Datos', tecnologia: 'Prisma 5 + PostgreSQL', justificacion: 'base dedicada por proyecto que aprovisiona el despliegue' },
    { id: 's3', capa: 'Pruebas', tecnologia: 'node:test con tsx', justificacion: 'sin dependencias extra' },
  ],
  integraciones: [{ id: 'i1', sistema: 'WhatsApp Business', proposito: 'enviar recordatorios a los vendedores', detalle: 'fuera del MVP; se evalúa después de la entrega', siFalla: 'no aplica en el MVP' }],
  decisiones: [
    { id: 'd1', decision: 'Los nombres del dominio van en español (Empresa, Contacto, Oportunidad, Actividad)', alternativas: 'inglés', justificacion: 'el cliente y el equipo trabajan en español' },
    { id: 'd2', decision: "Toda página que lee la base lleva export const dynamic = 'force-dynamic'", alternativas: 'generación estática', justificacion: 'durante el build no hay base de datos disponible' },
    { id: 'd3', decision: 'Server Actions para escribir; sin API REST', alternativas: 'rutas /api', justificacion: 'menos código y tipos compartidos' },
    { id: 'd4', decision: 'Sin autenticación en el MVP; usuarios y roles llegan en el sprint de endurecimiento', alternativas: 'login desde el inicio', justificacion: 'validar el flujo comercial primero' },
  ],
  seguridad: '', escalabilidad: '',
}
const ARQ = {
  nodes: [
    { id: 'n1', label: 'Navegador del vendedor', type: 'frontend', x: 60, y: 80 },
    { id: 'n2', label: 'Next.js (páginas y Server Actions)', type: 'backend', x: 320, y: 80 },
    { id: 'n3', label: 'Prisma Client', type: 'backend', x: 580, y: 80 },
    { id: 'n4', label: 'PostgreSQL del proyecto', type: 'database', x: 840, y: 80 },
  ],
  connections: [{ id: 'c1', from: 'n1', to: 'n2' }, { id: 'c2', from: 'n2', to: 'n3' }, { id: 'c3', from: 'n3', to: 'n4' }],
}

;(async () => {
  const sc = (await prisma.$queryRawUnsafe(`SELECT "solucionCode" c FROM "Solucion" WHERE id = $1`, SOL))[0].c
  const base = sc && sc.length >= 3 ? sc : 'PCRM-0001'
  const requisitos = R.map(r => ({ id: rid(r.k), tipo: 'historia', texto: r.texto, criterioAceptacion: r.crit, backlogItemId: null }))
  const prd = {
    estadoDocumento: 'EN_REVISION',
    resumenEjecutivo: 'CRM a la medida para el equipo comercial de Distribuidora Andina SAS (8 vendedores): empresas, contactos, oportunidades por etapas, actividades con recordatorios y tablero de KPIs.',
    problema: 'El seguimiento comercial vive en Excel y en WhatsApp personales: se pierden cotizaciones por falta de seguimiento, la jefa comercial no ve el pipeline y el historial se va con el vendedor.',
    objetivoGeneral: 'Centralizar la gestión comercial en una herramienta única, con visibilidad del pipeline en tiempo real.',
    objetivosEspecificos: [], dentroDeAlcance: [{ id: 'a1', texto: 'MVP: empresas, contactos, oportunidades (tablero), actividades y KPIs, sin autenticación.' }],
    fueraDeAlcance: [{ id: 'f1', texto: 'Integración con WhatsApp, facturación electrónica y app móvil.' }], personas: [], requisitos,
    requisitosNoFuncionales: [], metricas: [], riesgos: [], dependencias: [], supuestos: [], preguntasAbiertas: [],
  }
  await prisma.$executeRawUnsafe(`UPDATE "Solucion" SET prd = $2, "disenoTecnico" = $3, arquitectura = $4, "updatedAt" = NOW() WHERE id = $1`, SOL, JSON.stringify(prd), JSON.stringify(DISENO), JSON.stringify(ARQ))
  console.log('PRD, diseño técnico y diagrama guardados en la Solución')

  const [epic] = await prisma.$queryRawUnsafe(`INSERT INTO "Epic" (id, "solucionId", name, description, "startDate", "createdAt", "updatedAt") VALUES (gen_random_uuid()::text, $1, 'MVP del CRM', 'Primera versión usable del CRM: empresas, contactos, oportunidades, actividades y KPIs.', NOW(), NOW(), NOW()) RETURNING id`, SOL)
  const sprintCode = `${base}-0001-0001`
  const [sprint] = await prisma.$queryRawUnsafe(
    `INSERT INTO "Sprint" (id, "sprintCode", "solucionId", name, goal, "startDate", status, "epicId", "ownerAreaId", "responsibleName", metadata, "createdAt")
     VALUES (gen_random_uuid()::text, $1, $2, 'MVP', 'Entregar una versión que el cliente pueda probar en una demo.', NOW(), 'ACTIVE', $3, $4, 'Claude', '{"origen":"PREVENTA"}'::jsonb, NOW()) RETURNING id`,
    sprintCode, SOL, epic.id, AREA_DEV)
  let dep = null
  const ids = []
  for (const [i, r] of R.entries()) {
    const req = requisitos[i]
    const [t] = await prisma.$queryRawUnsafe(
      `INSERT INTO "BacklogItem" (id, title, description, type, priority, status, "sprintId", "solucionId", "areaId", "taskCode", "dependsOnTaskId", "prdRequisitoId", "order", "createdAt", "updatedAt")
       VALUES (gen_random_uuid()::text, $1, $2, 'FEATURE', 'HIGH', 'BACKLOG', $3, $4, $5, $6, $7, $8, $9, NOW(), NOW()) RETURNING id`,
      r.titulo, r.desc + '\n\n' + CONVENCIONES, sprint.id, SOL, AREA_DEV, `${sprintCode}-${String(i + 1).padStart(3, '0')}`, dep, req.id, i + 1)
    req.backlogItemId = t.id; ids.push(t.id); dep = t.id
  }
  await prisma.$executeRawUnsafe(`UPDATE "Solucion" SET prd = $2 WHERE id = $1`, SOL, JSON.stringify(prd))
  fs.writeFileSync('/root/crm_mvp.json', JSON.stringify({ sprintCode, sprintId: sprint.id, taskIds: ids }))
  console.log(`Sprint ${sprintCode} con ${ids.length} tareas encadenadas`)
  await prisma.$disconnect()
})()
