-- Copia del estado del motor de fases de un proyecto antes de reiniciarlo (por ejemplo, al cambiar su tipo de comercial a interno).
CREATE TABLE IF NOT EXISTS "ProyectoFaseRespaldo" (
  id text PRIMARY KEY,
  "solucionId" text NOT NULL REFERENCES "Solucion"(id) ON DELETE CASCADE,
  motivo text NOT NULL,
  datos jsonb NOT NULL,
  "createdAt" timestamp(3) NOT NULL DEFAULT NOW()
);
CREATE INDEX IF NOT EXISTS "ProyectoFaseRespaldo_sol_idx" ON "ProyectoFaseRespaldo" ("solucionId", "createdAt" DESC);
