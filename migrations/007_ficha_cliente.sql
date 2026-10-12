-- Ficha del cliente dentro del proyecto. Se apoya en la tabla Cliente que ya existe (la usan los leads) y le suma los datos de la empresa que faltaban.
ALTER TABLE "Cliente" ADD COLUMN IF NOT EXISTS "razonSocial" TEXT;
ALTER TABLE "Cliente" ADD COLUMN IF NOT EXISTS nit TEXT;
ALTER TABLE "Cliente" ADD COLUMN IF NOT EXISTS ciudad TEXT;
ALTER TABLE "Cliente" ADD COLUMN IF NOT EXISTS telefono TEXT;
ALTER TABLE "Cliente" ADD COLUMN IF NOT EXISTS "sitioWeb" TEXT;
ALTER TABLE "Cliente" ADD COLUMN IF NOT EXISTS tamano TEXT;
CREATE UNIQUE INDEX IF NOT EXISTS "Cliente_nit_key" ON "Cliente" (nit);

-- Personas de la empresa por papel: PRINCIPAL | DECISOR | REPRESENTANTE_LEGAL | FIRMANTE (una por papel)
CREATE TABLE IF NOT EXISTS "ClienteContacto" (
  id           text PRIMARY KEY,
  "clienteId"  text NOT NULL REFERENCES "Cliente"(id) ON DELETE CASCADE ON UPDATE CASCADE,
  rol          text NOT NULL,
  nombre       text NOT NULL DEFAULT '',
  cargo        text,
  correo       text,
  telefono     text,
  "createdAt"  timestamp(3) NOT NULL DEFAULT NOW(),
  "updatedAt"  timestamp(3) NOT NULL DEFAULT NOW(),
  UNIQUE ("clienteId", rol)
);

-- Lo propio de la oportunidad de cada proyecto: a qué empresa pertenece, de dónde vino y quién la lleva
CREATE TABLE IF NOT EXISTS "ProyectoFicha" (
  "solucionId"        text PRIMARY KEY REFERENCES "Solucion"(id) ON DELETE CASCADE ON UPDATE CASCADE,
  "clienteId"         text REFERENCES "Cliente"(id) ON DELETE SET NULL ON UPDATE CASCADE,
  fuente              text,
  presupuesto         double precision,
  "responsableId"     text,
  "responsableNombre" text,
  "actualizadoPor"    text,
  "updatedAt"         timestamp(3) NOT NULL DEFAULT NOW()
);

-- Carga inicial desde los leads (solo copia, no borra nada del lead)
INSERT INTO "ProyectoFicha" ("solucionId", "clienteId", fuente, "responsableId", "responsableNombre")
  SELECT s.id, l."clienteId", l.source, l."userId", u.name FROM "Solucion" s JOIN "Lead" l ON l.id = s."leadId" LEFT JOIN "User" u ON u.id = l."userId"
  ON CONFLICT ("solucionId") DO NOTHING;
INSERT INTO "ClienteContacto" (id, "clienteId", rol, nombre, correo, telefono)
  SELECT gen_random_uuid()::text, c.id, 'PRINCIPAL', c.contacto, c.email, (SELECT l.phone FROM "Lead" l WHERE l."clienteId" = c.id AND l.phone IS NOT NULL AND l.phone <> '' ORDER BY l."updatedAt" DESC LIMIT 1)
  FROM "Cliente" c
  ON CONFLICT ("clienteId", rol) DO NOTHING;
