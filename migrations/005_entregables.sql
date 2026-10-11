-- Entregables manuales marcados por una persona: { "<fase>": { "<clave>": { "ok", "por", "en", "evidencia" } } }
ALTER TABLE "ProyectoFase" ADD COLUMN IF NOT EXISTS entregables JSONB NOT NULL DEFAULT '{}'::jsonb;
