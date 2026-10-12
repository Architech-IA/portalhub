-- Notas y archivos por fase DENTRO del proyecto (sustituyen a las notas del Lead Hub, que desaparece).
CREATE TABLE IF NOT EXISTS "ProyectoNota" (
  id               text NOT NULL UNIQUE,
  "solucionId"     text NOT NULL REFERENCES "Solucion"(id) ON DELETE CASCADE ON UPDATE CASCADE,
  fase             text NOT NULL,
  contenido        text NOT NULL DEFAULT '',
  "actualizadoPor" text,
  "createdAt"      timestamp(3) NOT NULL DEFAULT NOW(),
  "updatedAt"      timestamp(3) NOT NULL DEFAULT NOW(),
  PRIMARY KEY ("solucionId", fase)
);
CREATE TABLE IF NOT EXISTS "ProyectoNotaArchivo" (
  id          text PRIMARY KEY,
  "notaId"    text NOT NULL REFERENCES "ProyectoNota"(id) ON DELETE CASCADE ON UPDATE CASCADE,
  nombre      text NOT NULL,
  size        integer NOT NULL DEFAULT 0,
  "mimeType"  text,
  base64      text NOT NULL,
  "subidoPor" text,
  "createdAt" timestamp(3) NOT NULL DEFAULT NOW()
);

-- Vistas con la forma que tenía el Lead Hub (por lead y etapa), para que lo que ya las leía (IA, contexto de la propuesta, entregables) siga funcionando con las notas del proyecto.
CREATE OR REPLACE VIEW "ProyectoNotaLead" AS
  SELECT n.id, s."leadId" AS "leadId", e.etapa AS phase, n.contenido AS content, n."actualizadoPor" AS "updatedBy", n."createdAt", n."updatedAt"
  FROM "ProyectoNota" n JOIN "Solucion" s ON s.id = n."solucionId"
  JOIN (VALUES ('identificacion', 'NEW'), ('contacto', 'CONTACTED'), ('diagnostico', 'DIAGNOSIS'), ('demo', 'DEMO_VALIDATION'), ('propuesta', 'PROPOSAL_SENT'), ('negociacion', 'NEGOTIATION')) AS e(fase, etapa) ON e.fase = n.fase
  WHERE s."leadId" IS NOT NULL;
CREATE OR REPLACE VIEW "ProyectoNotaArchivoLead" AS
  SELECT a.id, a."notaId" AS "hubId", a.nombre AS name, a.size, a."mimeType", a.base64, a."subidoPor" AS "uploadedBy", a."createdAt" FROM "ProyectoNotaArchivo" a;
