-- Piezas a medias del estándar SDD (incidentes y solicitudes de cambio, minuto en las actas). Migración ADITIVA.
-- Aplicar con psql:  psql "$DATABASE_URL" -f migrations/024_operacion_memoria.sql

ALTER TABLE "ActaItem" ADD COLUMN IF NOT EXISTS minuto text;

-- Incidentes y solicitudes de cambio de un proyecto en operación. Un incidente no se cierra con un parche suelto:
-- para cerrarlo debe quedar enlazado al requisito (REQ-###) que lo recoge y a la versión que lo corrige.
CREATE TABLE IF NOT EXISTS "CasoOperacion" (
  id             text PRIMARY KEY,
  "solucionId"   text NOT NULL,
  tipo           text NOT NULL CHECK (tipo IN ('INCIDENTE','SOLICITUD_CAMBIO')),
  codigo         text NOT NULL,
  titulo         text NOT NULL,
  descripcion    text NOT NULL DEFAULT '',
  severidad      text NOT NULL DEFAULT 'MEDIA' CHECK (severidad IN ('CRITICA','ALTA','MEDIA','BAJA')),
  estado         text NOT NULL DEFAULT 'ABIERTO' CHECK (estado IN ('ABIERTO','EN_ANALISIS','EN_CORRECCION','CERRADO','RECHAZADO')),
  "reqCodigo"    text,
  "propuestaId"  text,
  "versionCierre" text,
  facturable     boolean NOT NULL DEFAULT false,
  nota           text,
  "reportadoPor" text,
  "reportadoEn"  timestamp NOT NULL DEFAULT NOW(),
  "cerradoEn"    timestamp,
  "creadoPor"    text,
  "createdAt"    timestamp NOT NULL DEFAULT NOW(),
  "updatedAt"    timestamp NOT NULL DEFAULT NOW(),
  UNIQUE ("solucionId", codigo)
);
CREATE INDEX IF NOT EXISTS "CasoOperacion_sol_idx" ON "CasoOperacion" ("solucionId", estado);

-- El catálogo reconoce los tipos que ya tienen soporte propio.
UPDATE "ArtefactoTipo" SET fuente = '{"tipo":"casos","tipoCaso":"INCIDENTE"}'::jsonb, "updatedAt" = NOW() WHERE clave = 'incidente';
UPDATE "ArtefactoTipo" SET fuente = '{"tipo":"casos","tipoCaso":"SOLICITUD_CAMBIO"}'::jsonb, "updatedAt" = NOW() WHERE clave = 'solicitud_cambio';
UPDATE "ArtefactoTipo" SET fuente = '{"tipo":"decisiones"}'::jsonb, "updatedAt" = NOW() WHERE clave = 'decisiones';
