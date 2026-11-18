-- Documentos que se escriben y editan en el portal (Markdown) como entregable de una fase, con su historial de versiones.
CREATE TABLE IF NOT EXISTS "ProyectoDocumento" (
  id text PRIMARY KEY,
  "solucionId" text NOT NULL REFERENCES "Solucion"(id) ON DELETE CASCADE,
  "faseClave" text NOT NULL,
  "entregableClave" text NOT NULL,
  titulo text NOT NULL,
  contenido text NOT NULL DEFAULT '',
  version integer NOT NULL DEFAULT 1,
  propuesta text,
  "propuestaPor" text,
  "propuestaEn" timestamp(3),
  "actualizadoPor" text,
  "createdAt" timestamp(3) NOT NULL DEFAULT NOW(),
  "updatedAt" timestamp(3) NOT NULL DEFAULT NOW(),
  UNIQUE ("solucionId", "faseClave", "entregableClave")
);
CREATE TABLE IF NOT EXISTS "ProyectoDocumentoVersion" (
  id text PRIMARY KEY,
  "documentoId" text NOT NULL REFERENCES "ProyectoDocumento"(id) ON DELETE CASCADE,
  version integer NOT NULL,
  contenido text NOT NULL,
  nota text,
  "porNombre" text,
  "createdAt" timestamp(3) NOT NULL DEFAULT NOW()
);
CREATE INDEX IF NOT EXISTS "ProyectoDocumentoVersion_doc_idx" ON "ProyectoDocumentoVersion" ("documentoId", version);
