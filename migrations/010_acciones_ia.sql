-- Lo que la IA deja PENDIENTE de confirmación humana desde una sesión (notas tras consultar la web, tareas de backlog).
CREATE TABLE IF NOT EXISTS "ProyectoAccionIA" (
  id text PRIMARY KEY,
  "solucionId" text NOT NULL REFERENCES "Solucion"(id) ON DELETE CASCADE,
  "sesionId" text NOT NULL,
  tipo text NOT NULL,
  titulo text NOT NULL,
  payload jsonb NOT NULL,
  motivo text,
  estado text NOT NULL DEFAULT 'PENDIENTE',
  "decididoPor" text,
  "decididoEn" timestamp(3),
  "createdAt" timestamp(3) NOT NULL DEFAULT NOW()
);
CREATE INDEX IF NOT EXISTS "ProyectoAccionIA_sol_estado_idx" ON "ProyectoAccionIA" ("solucionId", estado);
