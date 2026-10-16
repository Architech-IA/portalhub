-- Archivos (Word, Excel, PDF) que la IA genera desde una sesión para un entregable manual.
-- Nacen como BORRADOR; solo cuentan como evidencia cuando una persona los acepta.
CREATE TABLE IF NOT EXISTS "ProyectoEntregableArchivo" (
  id text PRIMARY KEY,
  "solucionId" text NOT NULL REFERENCES "Solucion"(id) ON DELETE CASCADE,
  "faseClave" text NOT NULL,
  "entregableClave" text NOT NULL,
  titulo text NOT NULL,
  nombre text NOT NULL,
  mime text NOT NULL,
  size integer NOT NULL,
  contenido text NOT NULL,
  resumen text,
  estado text NOT NULL DEFAULT 'BORRADOR',
  "sesionId" text,
  "creadoPorNombre" text,
  "revisadoPorNombre" text,
  "revisadoEn" timestamp(3),
  "createdAt" timestamp(3) NOT NULL DEFAULT NOW()
);
CREATE INDEX IF NOT EXISTS "ProyectoEntregableArchivo_sol_fase_idx" ON "ProyectoEntregableArchivo" ("solucionId", "faseClave", "entregableClave");
