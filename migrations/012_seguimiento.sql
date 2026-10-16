-- Avisos automáticos de cada proyecto y registro del uso de tokens de las llamadas al modelo que no son una sesión de chat.
CREATE TABLE IF NOT EXISTS "ProyectoAlerta" (
  id text PRIMARY KEY,
  "solucionId" text NOT NULL REFERENCES "Solucion"(id) ON DELETE CASCADE,
  tipo text NOT NULL,
  ref text NOT NULL,
  titulo text NOT NULL,
  detalle text NOT NULL DEFAULT '',
  estado text NOT NULL DEFAULT 'ABIERTA',
  "creadaEn" timestamp(3) NOT NULL DEFAULT NOW(),
  "resueltaEn" timestamp(3),
  UNIQUE ("solucionId", tipo, ref)
);
CREATE INDEX IF NOT EXISTS "ProyectoAlerta_sol_estado_idx" ON "ProyectoAlerta" ("solucionId", estado);
CREATE TABLE IF NOT EXISTS "UsoIA" (
  id text PRIMARY KEY,
  clave text NOT NULL,
  origen text NOT NULL,
  "promptTokens" integer NOT NULL DEFAULT 0,
  "cachedTokens" integer NOT NULL DEFAULT 0,
  "completionTokens" integer NOT NULL DEFAULT 0,
  "reasoningTokens" integer NOT NULL DEFAULT 0,
  "createdAt" timestamp(3) NOT NULL DEFAULT NOW()
);
CREATE INDEX IF NOT EXISTS "UsoIA_clave_idx" ON "UsoIA" (clave);
