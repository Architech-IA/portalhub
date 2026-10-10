// Fases 7-8: PRD del proyecto aprobado (v2), diseño técnico actualizado y backlog del sprint de endurecimiento (usuarios, roles y celular).
const fs = require('fs')
const path = require('path')
const ROOT = '/root/portal-architechia'
const { PrismaClient } = require(path.join(ROOT, 'node_modules/@prisma/client'))
const prisma = new PrismaClient()
const IDS = JSON.parse(fs.readFileSync('/root/crm_ids.json', 'utf8'))
const SOL = IDS.solucionId
const AREA_DEV = '947ca771-fe9e-4c3f-bfea-2ef2e27986c6'
const rid = (k) => 'r' + k + Math.random().toString(36).slice(2, 6)
const CONVENCIONES = 'Convenciones del proyecto (están en el Diseño técnico): nombres del dominio en español; páginas y acciones usan Prisma desde "@/lib/db"; toda página que lee la base lleva `export const dynamic = \'force-dynamic\'`; formularios con Server Actions ("use server"); el esquema de Prisma YA incluye el modelo Usuario (rol VENDEDOR o JEFATURA, claveHash) y Actividad.responsable: no cambies prisma/schema.prisma ni package.json. Cada archivo nuevo debe compilar con `npx tsc --noEmit` y las pruebas nuevas deben pasar con `npm test`.'

const H = [
  { k: 'ses', texto: 'Como jefa comercial quiero que las claves se guarden de forma segura y que la sesión viaje en una cookie firmada, para que nadie pueda hacerse pasar por otro usuario.',
    crit: 'Existen src/lib/clave.ts (hashClave y verificarClave con scrypt de node:crypto y sal aleatoria) y src/lib/sesion.ts (crear y leer un token firmado con HMAC SHA256 y SESSION_SECRET, con expiración); tests/clave.test.ts y tests/sesion.test.ts pasan, incluyendo token manipulado y token vencido.',
    titulo: 'Claves con scrypt y sesión firmada (HMAC) con pruebas',
    desc: 'Crea src/lib/clave.ts: hashClave(clave: string): string que devuelve "scrypt$<sal hex>$<hash hex>" usando scryptSync de node:crypto con sal aleatoria de 16 bytes y clave derivada de 64 bytes, y verificarClave(clave: string, almacenado: string): boolean que usa timingSafeEqual y devuelve false ante cualquier formato inválido. Crea src/lib/sesion.ts: tipo Sesion = { id: string; email: string; nombre: string; rol: "VENDEDOR" | "JEFATURA"; exp: number } (exp en segundos Unix), crearToken(sesion, secreto): string = base64url(JSON) + "." + base64url(HMAC-SHA256), leerToken(token, secreto): Sesion | null (null si la firma no coincide, el formato es inválido o ya venció) y la constante COOKIE_SESION = "sesion" y DURACION_SESION_S = 8 * 3600. El secreto se pasa por parámetro (en la app sale de process.env.SESSION_SECRET). Crea tests/clave.test.ts y tests/sesion.test.ts con node:test: hash distinto cada vez, verificación correcta e incorrecta, formato inválido; token válido, token con el payload alterado, token con otra firma y token vencido.' },
  { k: 'log', texto: 'Como jefa comercial quiero iniciar sesión con mi correo y mi clave y crear usuarios del equipo con rol vendedor o jefatura, y que sin sesión no se pueda ver nada.',
    crit: 'La ruta /login permite entrar con correo y clave; si no existe ningún usuario ofrece crear la jefatura inicial; /usuarios (solo JEFATURA) lista y crea usuarios; un middleware redirige a /login a quien no tenga sesión válida; el menú muestra el nombre y un botón Salir.',
    titulo: 'Login, usuarios y protección de rutas con middleware',
    desc: 'Crea: (1) src/app/login/page.tsx con formulario de correo y clave y su Server Action iniciarSesion (busca el Usuario por email, verifica la clave con verificarClave, crea el token con crearToken y SESSION_SECRET, lo guarda en la cookie httpOnly "sesion", sameSite lax, secure en producción, y redirige a "/"; si falla muestra "Correo o clave incorrectos"). Si NO existe ningún usuario en la base, la misma página muestra en su lugar un formulario "Crear la jefatura inicial" (nombre, correo, clave de mínimo 8 caracteres) con su Server Action que crea el primer Usuario con rol JEFATURA e inicia sesión. (2) src/app/usuarios/page.tsx: solo para rol JEFATURA (si no, redirige a "/"): tabla de usuarios (nombre, correo, rol) y formulario para crear usuarios con rol VENDEDOR o JEFATURA y clave inicial de mínimo 8 caracteres (hashClave). (3) src/lib/sesion-edge.ts: función async verificarTokenEdge(token, secreto): Promise<Sesion | null> equivalente a leerToken pero usando Web Crypto (crypto.subtle HMAC SHA-256), porque el middleware de Next corre en el runtime Edge y no puede usar node:crypto. (4) src/middleware.ts: para toda ruta excepto /login, /_next/*, /icon.svg y archivos estáticos, exige la cookie "sesion" válida (verificarTokenEdge con process.env.SESSION_SECRET); si no, redirige a /login. (5) Actualiza src/components/Nav.tsx para mostrar el nombre de quien tiene sesión, el enlace a Usuarios solo si es JEFATURA y un botón Salir (Server Action que borra la cookie). Para leer la sesión en Server Components crea el helper src/lib/sesion-servidor.ts: obtenerSesion() que lee la cookie con cookies() de "next/headers" y usa leerToken.' },
  { k: 'rol', texto: 'Como vendedor quiero ver solo mis empresas, oportunidades y actividades; como jefa comercial quiero verlo todo y que los KPIs respeten ese alcance.',
    crit: 'Las funciones de src/lib/crm.ts reciben el alcance (rol y correo) y, para VENDEDOR, filtran por vendedor/responsable igual a su correo; las pantallas y los KPIs usan la sesión; tests/alcance.test.ts cubre el filtro puro.',
    titulo: 'Alcance por rol en datos, pantallas y KPIs',
    desc: 'En src/lib/alcance.ts exporta el tipo Alcance = { rol: "VENDEDOR" | "JEFATURA"; email: string } y las funciones puras filtroEmpresas(alcance), filtroOportunidades(alcance) y filtroActividades(alcance) que devuelven el objeto `where` de Prisma: para JEFATURA {} y para VENDEDOR { vendedor: email } en Empresa y Oportunidad, y { responsable: email } en Actividad. Actualiza src/lib/crm.ts para que listarEmpresas, listarOportunidades, listarActividades y kpis reciban el Alcance como primer parámetro y apliquen esos filtros, y para que crearEmpresa, crearOportunidad y crearActividad asignen vendedor/responsable = email del VENDEDOR cuando quien crea es un vendedor (la jefatura puede indicarlo). Actualiza todas las páginas y acciones para obtener la sesión con obtenerSesion() de src/lib/sesion-servidor.ts y pasar el alcance. Crea tests/alcance.test.ts con node:test para los filtros puros. En el formulario de actividades agrega el campo opcional responsable solo para la jefatura.' },
  { k: 'cel', texto: 'Como vendedor quiero usar el CRM desde el celular sin que la pantalla se desborde, y que el sitio no dé errores por recursos faltantes.',
    crit: 'En un ancho de 390 px ninguna página se desborda horizontalmente (el tablero kanban hace scroll dentro de su contenedor) y no hay errores 404 de recursos como el ícono.',
    titulo: 'Diseño adaptable para celular y ícono del sitio',
    desc: 'Ajusta src/app/layout.tsx, src/components/Nav.tsx y las páginas para que en 390 px de ancho no haya desborde horizontal de la página: la barra de navegación debe poder envolverse en varias líneas, los formularios y tablas deben respetar el ancho (box-sizing: border-box, max-width: 100%, las tablas dentro de un contenedor con overflow-x: auto) y el tablero kanban de oportunidades debe hacer scroll horizontal SOLO dentro de su propio contenedor. Crea src/app/icon.svg (un ícono simple del CRM) para eliminar el 404 del favicon. No cambies la lógica.' },
]

;(async () => {
  const sc = (await prisma.$queryRawUnsafe(`SELECT "solucionCode" c FROM "Solucion" WHERE id = $1`, SOL))[0].c
  const [row] = await prisma.$queryRawUnsafe(`SELECT prd, "disenoTecnico" d FROM "Solucion" WHERE id = $1`, SOL)
  const prd = JSON.parse(row.prd); const dis = JSON.parse(row.d)
  const nuevos = H.map(h => ({ id: rid(h.k), tipo: 'historia', texto: h.texto, criterioAceptacion: h.crit, backlogItemId: null }))
  prd.requisitos = prd.requisitos.concat(nuevos)
  prd.estadoDocumento = 'APROBADO'
  prd.resumenEjecutivo = 'PRD v2 del proyecto. ' + prd.resumenEjecutivo + ' Alcance acordado: MVP (hecho) más usuarios con roles, adaptación a celular y entrega con capacitación.'
  prd.dentroDeAlcance = prd.dentroDeAlcance.concat([{ id: 'a2', texto: 'Usuarios y roles (vendedor / jefatura) con inicio de sesión propio.' }, { id: 'a3', texto: 'Adaptación a celular y manual de usuario.' }])
  // Diseño técnico actualizado: lo hace una persona al acordar el alcance (cambio de diseño respecto del MVP)
  const act = dis.entidades.find(e => e.nombre === 'Actividad'); act.atributos += ', responsable (correo del vendedor)'
  const usr = dis.entidades.find(e => e.nombre === 'Usuario')
  usr.atributos = 'id, nombre, email (único), rol (VENDEDOR, JEFATURA), claveHash (scrypt), creadoEn'
  usr.relaciones = 'sin relaciones de base de datos: el vendedor se identifica por su correo en Empresa.vendedor, Oportunidad.vendedor y Actividad.responsable'
  dis.decisiones = dis.decisiones.filter(d => d.id !== 'd4').concat([
    { id: 'd4', decision: 'Autenticación propia: claves con scrypt (node:crypto) y sesión en cookie httpOnly firmada con HMAC SHA256 (SESSION_SECRET)', alternativas: 'NextAuth, proveedor externo', justificacion: 'sin dependencias nuevas y suficiente para un equipo de 10 personas' },
    { id: 'd5', decision: 'El middleware valida la cookie con Web Crypto porque corre en el runtime Edge', alternativas: 'validar en cada página', justificacion: 'protege todas las rutas desde un solo punto' },
    { id: 'd6', decision: 'El alcance por rol se aplica en la capa de datos (src/lib/crm.ts), no en las pantallas', alternativas: 'filtrar en cada página', justificacion: 'una sola regla, imposible de olvidar' },
  ])
  dis.estadoDocumento = 'APROBADO'

  const [epic] = await prisma.$queryRawUnsafe(`INSERT INTO "Epic" (id, "solucionId", name, description, "startDate", "createdAt", "updatedAt") VALUES (gen_random_uuid()::text, $1, 'Endurecimiento: usuarios, roles y celular', 'Lo que falta para entregar: acceso con usuarios, alcance por rol y adaptación a celular.', NOW(), NOW(), NOW()) RETURNING id`, SOL)
  const sprintCode = `${sc}-0002-0001`
  const [sprint] = await prisma.$queryRawUnsafe(`INSERT INTO "Sprint" (id, "sprintCode", "solucionId", name, goal, "startDate", status, "epicId", "ownerAreaId", "responsibleName", "createdAt") VALUES (gen_random_uuid()::text, $1, $2, 'Endurecimiento', 'Usuarios con roles y celular, listo para QA.', NOW(), 'ACTIVE', $3, $4, 'Claude', NOW()) RETURNING id`, sprintCode, SOL, epic.id, AREA_DEV)
  let dep = null; const ids = []
  for (const [i, h] of H.entries()) {
    const [t] = await prisma.$queryRawUnsafe(
      `INSERT INTO "BacklogItem" (id, title, description, type, priority, status, "sprintId", "solucionId", "areaId", "taskCode", "dependsOnTaskId", "prdRequisitoId", "order", "createdAt", "updatedAt")
       VALUES (gen_random_uuid()::text, $1, $2, 'FEATURE', 'HIGH', 'BACKLOG', $3, $4, $5, $6, $7, $8, $9, NOW(), NOW()) RETURNING id`,
      h.titulo, h.desc + '\n\n' + CONVENCIONES, sprint.id, SOL, AREA_DEV, `${sprintCode}-${String(i + 1).padStart(3, '0')}`, dep, nuevos[i].id, i + 1)
    nuevos[i].backlogItemId = t.id; ids.push(t.id); dep = t.id
  }
  await prisma.$executeRawUnsafe(`UPDATE "Solucion" SET prd = $2, "disenoTecnico" = $3, "updatedAt" = NOW() WHERE id = $1`, SOL, JSON.stringify(prd), JSON.stringify(dis))
  fs.writeFileSync('/root/crm_hard.json', JSON.stringify({ sprintCode, sprintId: sprint.id, taskIds: ids }))
  console.log(`PRD v2 aprobado (${prd.requisitos.length} requisitos), diseño actualizado y sprint ${sprintCode} con ${ids.length} tareas`)
  await prisma.$disconnect()
})()
