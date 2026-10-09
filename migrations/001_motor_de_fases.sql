-- Motor de fases (MASD-0024): estado del ciclo de vida de un proyecto, desde el lead hasta la entrega.
-- Aditivo y repetible: solo crea tablas nuevas; no toca ninguna existente. Prisma no las conoce,
-- las lee y escribe únicamente portalhub (src/routes/fases.rs).

CREATE TABLE IF NOT EXISTS "ProyectoFase" (
  "solucionId"  text PRIMARY KEY REFERENCES "Solucion"(id) ON DELETE CASCADE,
  plantilla     text NOT NULL,
  -- copia de la plantilla al iniciar: cambiar la plantilla después no altera proyectos en curso
  definicion    jsonb NOT NULL,
  "faseActual"  text NOT NULL,
  estado        text NOT NULL DEFAULT 'EN_CURSO',   -- EN_CURSO | CERRADO_PERDIDO | COMPLETADO
  -- { "<fase>": [ { "ok": bool, "por": text|null, "en": timestamp|null } ... ] } en el orden de la plantilla
  criterios     jsonb NOT NULL DEFAULT '{}'::jsonb,
  "createdAt"   timestamp(3) NOT NULL DEFAULT NOW(),
  "updatedAt"   timestamp(3) NOT NULL DEFAULT NOW()
);

CREATE TABLE IF NOT EXISTS "ProyectoFaseHistorial" (
  id               text PRIMARY KEY,
  "solucionId"     text NOT NULL REFERENCES "Solucion"(id) ON DELETE CASCADE,
  fase             text NOT NULL,
  accion           text NOT NULL,  -- INICIO | AVANCE | RETROCESO | SINCRONIZADO | VENTA_CONFIRMADA | VENTA_PERDIDA | COMPLETADO
  "usuarioId"      text,
  "usuarioNombre"  text,
  nota             text,
  "createdAt"      timestamp(3) NOT NULL DEFAULT NOW()
);
CREATE INDEX IF NOT EXISTS "ProyectoFaseHistorial_sol_idx" ON "ProyectoFaseHistorial" ("solucionId", "createdAt" DESC);

CREATE TABLE IF NOT EXISTS "FaseActividad" (
  id               text PRIMARY KEY,
  "solucionId"     text NOT NULL REFERENCES "Solucion"(id) ON DELETE CASCADE,
  fase             text NOT NULL,
  clave            text NOT NULL,
  "backlogItemId"  text,
  "createdAt"      timestamp(3) NOT NULL DEFAULT NOW(),
  UNIQUE ("solucionId", fase, clave)
);

-- Versiones del PRD: al confirmar la venta se guarda el PRD de preventa antes de reescribirlo.
CREATE TABLE IF NOT EXISTS "SolucionPrdVersion" (
  id            text PRIMARY KEY,
  "solucionId"  text NOT NULL REFERENCES "Solucion"(id) ON DELETE CASCADE,
  version       integer NOT NULL,
  contenido     text NOT NULL,
  motivo        text,
  "usuarioId"   text,
  "createdAt"   timestamp(3) NOT NULL DEFAULT NOW(),
  UNIQUE ("solucionId", version)
);
