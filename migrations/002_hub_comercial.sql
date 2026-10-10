-- Hub de la solución para proyectos comerciales (MASD-0025): solo aditivo y repetible.
-- Columnas nuevas con valor por defecto (Prisma las ignora y sigue funcionando) y tablas nuevas que solo lee y escribe portalhub.

-- Una Solución adicional (fase 2, mantenimiento) cuelga del proyecto original; leadId sigue siendo único.
ALTER TABLE "Solucion" ADD COLUMN IF NOT EXISTS "parentId" text REFERENCES "Solucion"(id) ON DELETE SET NULL;
-- Enlace de solo lectura para el cliente (32 caracteres hexadecimales aleatorios; NULL = sin enlace).
ALTER TABLE "Solucion" ADD COLUMN IF NOT EXISTS "tokenCliente" text;
CREATE UNIQUE INDEX IF NOT EXISTS "Solucion_tokenCliente_key" ON "Solucion" ("tokenCliente") WHERE "tokenCliente" IS NOT NULL;
-- Diagramas Mermaid: [{ id, titulo, tipo: er|secuencia|c4|flujo, codigo }]
ALTER TABLE "Solucion" ADD COLUMN IF NOT EXISTS diagramas text DEFAULT '[]';

-- Hitos con calendario de pagos, avance real (sprint) y aceptación del cliente.
ALTER TABLE "Hito" ADD COLUMN IF NOT EXISTS monto double precision NOT NULL DEFAULT 0;
ALTER TABLE "Hito" ADD COLUMN IF NOT EXISTS "estadoPago" text NOT NULL DEFAULT 'PENDIENTE';   -- PENDIENTE | FACTURADO | PAGADO
ALTER TABLE "Hito" ADD COLUMN IF NOT EXISTS "sprintId" text REFERENCES "Sprint"(id) ON DELETE SET NULL;
ALTER TABLE "Hito" ADD COLUMN IF NOT EXISTS "aceptadoPor" text;
ALTER TABLE "Hito" ADD COLUMN IF NOT EXISTS "aceptadoEn" timestamp(3);
ALTER TABLE "Hito" ADD COLUMN IF NOT EXISTS "aceptadoNota" text;

-- Control de cambios de alcance.
CREATE TABLE IF NOT EXISTS "SolicitudCambio" (
  id                text PRIMARY KEY,
  "solucionId"      text NOT NULL REFERENCES "Solucion"(id) ON DELETE CASCADE,
  titulo            text NOT NULL,
  descripcion       text,
  "impactoAlcance"  text,
  "impactoCosto"    double precision NOT NULL DEFAULT 0,
  "impactoDias"     integer NOT NULL DEFAULT 0,
  estado            text NOT NULL DEFAULT 'SOLICITADA',  -- SOLICITADA | EN_EVALUACION | APROBADA | RECHAZADA | IMPLEMENTADA
  "solicitadoPor"   text,
  "solicitadoPorId" text,
  "decididoPor"     text,
  "decididoEn"      timestamp(3),
  "decisionNota"    text,
  aplicado          boolean NOT NULL DEFAULT false,      -- true = el costo ya se sumó al valor del proyecto
  "createdAt"       timestamp(3) NOT NULL DEFAULT NOW(),
  "updatedAt"       timestamp(3) NOT NULL DEFAULT NOW()
);
CREATE INDEX IF NOT EXISTS "SolicitudCambio_sol_idx" ON "SolicitudCambio" ("solucionId", "createdAt" DESC);

-- Fotografías de todos los documentos: línea base (PRD y diseño aprobados), as-built (entrega) o manual.
CREATE TABLE IF NOT EXISTS "SolucionSnapshot" (
  id                 text PRIMARY KEY,
  "solucionId"       text NOT NULL REFERENCES "Solucion"(id) ON DELETE CASCADE,
  etiqueta           text NOT NULL,
  tipo               text NOT NULL,                        -- LINEA_BASE | AS_BUILT | MANUAL
  contenido          jsonb NOT NULL,
  "creadoPorId"      text,
  "creadoPorNombre"  text,
  "createdAt"        timestamp(3) NOT NULL DEFAULT NOW()
);
CREATE INDEX IF NOT EXISTS "SolucionSnapshot_sol_idx" ON "SolucionSnapshot" ("solucionId", "createdAt" DESC);
