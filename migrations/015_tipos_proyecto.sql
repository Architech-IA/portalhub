-- Tipos de proyecto editables (antes estaban fijos en el código). Cada tipo tiene una naturaleza (comercial o interna), de la que sale su plantilla de fases.
-- Los cinco tipos de siempre quedan como tipos del sistema: se pueden renombrar y reordenar, pero no borrar ni desactivar.
CREATE TABLE IF NOT EXISTS "TipoProyecto" (
  clave text PRIMARY KEY,
  etiqueta text NOT NULL,
  naturaleza text NOT NULL CHECK (naturaleza IN ('COMERCIAL', 'INTERNO')),
  orden integer NOT NULL DEFAULT 0,
  activo boolean NOT NULL DEFAULT true,
  sistema boolean NOT NULL DEFAULT false,
  "createdAt" timestamp(3) NOT NULL DEFAULT NOW()
);
INSERT INTO "TipoProyecto" (clave, etiqueta, naturaleza, orden, sistema) VALUES
  ('PROJECT', 'Proyecto', 'COMERCIAL', 10, true),
  ('DEMO', 'Demo', 'COMERCIAL', 20, true),
  ('PARTNERSHIP', 'Alianza', 'COMERCIAL', 30, true),
  ('PRODUCT', 'Producto propio', 'INTERNO', 40, true),
  ('INTERN', 'Mejora interna', 'INTERNO', 50, true)
ON CONFLICT (clave) DO NOTHING;
