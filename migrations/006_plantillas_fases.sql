-- Ajustes del editor de plantillas de fases: se superponen a la plantilla de fábrica (plantillas/*.json).
-- fases: { "<fase>": { objetivo, aprobador, criterios, entregables, requeridos, sesiones } }
CREATE TABLE IF NOT EXISTS "PlantillaFasesAjuste" (
  codigo           text PRIMARY KEY,
  fases            jsonb NOT NULL DEFAULT '{}'::jsonb,
  version          integer NOT NULL DEFAULT 1,
  "actualizadoPor" text,
  "updatedAt"      timestamp(3) NOT NULL DEFAULT NOW()
);
CREATE TABLE IF NOT EXISTS "PlantillaFasesHistorial" (
  id               text PRIMARY KEY,
  codigo           text NOT NULL,
  fases            jsonb NOT NULL,
  version          integer NOT NULL,
  nota             text,
  "usuarioNombre"  text,
  "createdAt"      timestamp(3) NOT NULL DEFAULT NOW()
);
CREATE INDEX IF NOT EXISTS "PlantillaFasesHistorial_codigo_idx" ON "PlantillaFasesHistorial" (codigo, "createdAt" DESC);
