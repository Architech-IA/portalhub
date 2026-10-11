-- Cada sesión puede tener un rol dentro de una fase (catálogo en las plantillas) y una lista de cierre.
ALTER TABLE "ProyectoSesion" ADD COLUMN IF NOT EXISTS "faseClave" TEXT;
ALTER TABLE "ProyectoSesion" ADD COLUMN IF NOT EXISTS rol TEXT;
ALTER TABLE "ProyectoSesion" ADD COLUMN IF NOT EXISTS cierre JSONB NOT NULL DEFAULT '[]'::jsonb;
