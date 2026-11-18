-- Catálogo de demos, MVPs y casos (vitrina pública + registro interno).
CREATE TABLE IF NOT EXISTS "DemoCatalogo" (
  id text PRIMARY KEY,
  slug text NOT NULL UNIQUE,
  nombre text NOT NULL,
  resumen text NOT NULL DEFAULT '',
  sector text NOT NULL DEFAULT '',
  servicio text NOT NULL DEFAULT 'otro',
  tipo text NOT NULL DEFAULT 'DEMO',
  url text,
  icono text NOT NULL DEFAULT 'sparkles',
  publico boolean NOT NULL DEFAULT false,
  orden integer NOT NULL DEFAULT 0,
  "solucionId" text REFERENCES "Solucion"(id) ON DELETE SET NULL,
  vivo boolean,
  "verificadoAt" timestamp(3),
  "createdAt" timestamp(3) NOT NULL DEFAULT NOW(),
  "updatedAt" timestamp(3) NOT NULL DEFAULT NOW()
)
