-- 025: sistema multiagente (MASD-0028). Solo aditivo: no toca Agent, Area, AgenteMemoria, AgenteHilo ni AgenteTraspaso.

-- ── Identidad y responsabilidad ──────────────────────────────────────────────────────────────────
CREATE TABLE IF NOT EXISTS "AgenteFicha" (
  slug              text PRIMARY KEY REFERENCES "Agent"(slug) ON DELETE CASCADE ON UPDATE CASCADE,
  alma              jsonb NOT NULL DEFAULT '{}'::jsonb,           -- {mision, tono, valores[], limites[], ejemplos[]}
  alcance           text  NOT NULL DEFAULT '',                    -- de qué es responsable (y de qué no)
  "superiorSlug"    text  REFERENCES "Agent"(slug) ON DELETE SET NULL ON UPDATE CASCADE,
  herramientas      text[] NOT NULL DEFAULT '{}',                 -- acciones que puede intentar
  autonomia         text  NOT NULL DEFAULT 'ASISTIDA' CHECK (autonomia IN ('ASISTIDA','SUPERVISADA','AUTONOMA')),
  "autonomiaPorAccion" jsonb NOT NULL DEFAULT '{}'::jsonb,        -- {accion: nivel} para anular el nivel general
  version           integer NOT NULL DEFAULT 1,
  "actualizadoPor"  text,
  "updatedAt"       timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE IF NOT EXISTS "AgenteFichaHistorial" (
  id        text PRIMARY KEY DEFAULT gen_random_uuid()::text,
  slug      text NOT NULL,
  version   integer NOT NULL,
  cambio    jsonb NOT NULL,
  por       text,
  "creadoEn" timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS "AgenteFichaHistorial_slug_idx" ON "AgenteFichaHistorial"(slug, "creadoEn" DESC);

-- Con quién se comunica cada agente.
CREATE TABLE IF NOT EXISTS "AgenteComunicacion" (
  id                   text PRIMARY KEY DEFAULT gen_random_uuid()::text,
  "agenteSlug"         text NOT NULL REFERENCES "Agent"(slug) ON DELETE CASCADE ON UPDATE CASCADE,
  "tipoDestino"        text NOT NULL CHECK ("tipoDestino" IN ('AGENTE','PERSONA','ROL','AREA','EXTERNO')),
  destino              text NOT NULL,                              -- slug, id de persona, rol (EQUIPO, CLIENTE...), id de área o etiqueta
  direccion            text NOT NULL DEFAULT 'AMBAS' CHECK (direccion IN ('ENTRADA','SALIDA','AMBAS')),
  canales              text[] NOT NULL DEFAULT '{}',               -- vacío = cualquiera
  "requiereAprobacion" boolean NOT NULL DEFAULT false,
  nota                 text,
  "creadoEn"           timestamptz NOT NULL DEFAULT now(),
  UNIQUE ("agenteSlug", "tipoDestino", destino, direccion)
);

-- ── Gateway: modelo único de mensajería ─────────────────────────────────────────────────────────
CREATE TABLE IF NOT EXISTS "GwPersona" (
  id          text PRIMARY KEY DEFAULT gen_random_uuid()::text,
  nombre      text NOT NULL,
  tipo        text NOT NULL DEFAULT 'EXTERNO' CHECK (tipo IN ('EQUIPO','CLIENTE','EXTERNO')),
  "userId"    text REFERENCES "User"(id) ON DELETE SET NULL,
  "creadoEn"  timestamptz NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX IF NOT EXISTS "GwPersona_userId_key" ON "GwPersona"("userId") WHERE "userId" IS NOT NULL;

CREATE TABLE IF NOT EXISTS "GwIdentidad" (
  id          text PRIMARY KEY DEFAULT gen_random_uuid()::text,
  "personaId" text NOT NULL REFERENCES "GwPersona"(id) ON DELETE CASCADE,
  canal       text NOT NULL CHECK (canal IN ('TELEGRAM','CORREO','PORTAL')),
  "externoId" text NOT NULL,                                       -- id de Telegram, correo, id de usuario del portal
  verificada  boolean NOT NULL DEFAULT false,
  "creadoEn"  timestamptz NOT NULL DEFAULT now(),
  UNIQUE (canal, "externoId")
);

CREATE TABLE IF NOT EXISTS "GwHilo" (
  id              text PRIMARY KEY DEFAULT gen_random_uuid()::text,
  canal           text NOT NULL CHECK (canal IN ('TELEGRAM','CORREO','PORTAL')),
  "personaId"     text REFERENCES "GwPersona"(id) ON DELETE SET NULL,
  "agenteSlug"    text REFERENCES "Agent"(slug) ON DELETE SET NULL ON UPDATE CASCADE,
  "solucionId"    text,
  asunto          text,
  estado          text NOT NULL DEFAULT 'ABIERTO' CHECK (estado IN ('ABIERTO','CERRADO')),
  "canalHiloId"   text,                                            -- id de chat de Telegram, conversationId del correo, id de sesión del portal
  "creadoEn"      timestamptz NOT NULL DEFAULT now(),
  "ultimoEn"      timestamptz NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX IF NOT EXISTS "GwHilo_canal_hilo_key" ON "GwHilo"(canal, "canalHiloId", "agenteSlug") WHERE "canalHiloId" IS NOT NULL;
CREATE INDEX IF NOT EXISTS "GwHilo_agente_idx" ON "GwHilo"("agenteSlug", "ultimoEn" DESC);
CREATE INDEX IF NOT EXISTS "GwHilo_persona_idx" ON "GwHilo"("personaId", "ultimoEn" DESC);

CREATE TABLE IF NOT EXISTS "GwMensaje" (
  id                 text PRIMARY KEY DEFAULT gen_random_uuid()::text,
  "hiloId"           text NOT NULL REFERENCES "GwHilo"(id) ON DELETE CASCADE,
  direccion          text NOT NULL CHECK (direccion IN ('ENTRADA','SALIDA','INTERNO')),
  "autorTipo"        text NOT NULL CHECK ("autorTipo" IN ('PERSONA','AGENTE','SISTEMA')),
  "personaId"        text REFERENCES "GwPersona"(id) ON DELETE SET NULL,
  "agenteSlug"       text,
  texto              text NOT NULL,
  adjuntos           jsonb NOT NULL DEFAULT '[]'::jsonb,
  "canalMensajeId"   text,
  "respondeA"        text,
  estado             text NOT NULL DEFAULT 'ENTREGABLE' CHECK (estado IN ('ENTREGABLE','PENDIENTE_APROBACION','RECHAZADO','ENTREGADO')),
  "creadoEn"         timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS "GwMensaje_hilo_idx" ON "GwMensaje"("hiloId", "creadoEn");
CREATE UNIQUE INDEX IF NOT EXISTS "GwMensaje_canalmsg_key" ON "GwMensaje"("hiloId", "canalMensajeId") WHERE "canalMensajeId" IS NOT NULL;

-- ── Memoria en capas ───────────────────────────────────────────────────────────────────────────
CREATE TABLE IF NOT EXISTS "GwMemoria" (
  id                text PRIMARY KEY DEFAULT gen_random_uuid()::text,
  ambito            text NOT NULL CHECK (ambito IN ('CONVERSACION','AGENTE','PROYECTO')),
  "refId"           text NOT NULL,                                 -- id del hilo, slug del agente o id de la solución
  clave             text NOT NULL DEFAULT '',
  contenido         text NOT NULL,
  "origenMensajeId" text,
  vigente           boolean NOT NULL DEFAULT true,
  "creadoEn"        timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS "GwMemoria_ref_idx" ON "GwMemoria"(ambito, "refId", vigente, "creadoEn" DESC);

-- ── Trazabilidad ───────────────────────────────────────────────────────────────────────────────
CREATE TABLE IF NOT EXISTS "GwDecision" (
  id                  text PRIMARY KEY DEFAULT gen_random_uuid()::text,
  "mensajeId"         text REFERENCES "GwMensaje"(id) ON DELETE CASCADE,
  "hiloId"            text REFERENCES "GwHilo"(id) ON DELETE CASCADE,
  "agenteSlug"        text NOT NULL,
  accion              text NOT NULL DEFAULT 'responder',
  decision            text NOT NULL,                               -- qué decidió y por qué
  fuentes             jsonb NOT NULL DEFAULT '[]'::jsonb,          -- [{tipo, id, ref}]
  "autonomiaAplicada" text NOT NULL,
  estado              text NOT NULL DEFAULT 'EJECUTADA' CHECK (estado IN ('EJECUTADA','PENDIENTE_APROBACION','RECHAZADA')),
  "decididoPor"       text,
  "decididoEn"        timestamptz,
  "creadoEn"          timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS "GwDecision_agente_idx" ON "GwDecision"("agenteSlug", "creadoEn" DESC);
CREATE INDEX IF NOT EXISTS "GwDecision_pend_idx" ON "GwDecision"(estado) WHERE estado = 'PENDIENTE_APROBACION';

-- ── Siembra: ficha de los agentes que ya existen ───────────────────────────────────────────────
INSERT INTO "AgenteFicha" (slug, alma, alcance, "superiorSlug", herramientas, autonomia, "actualizadoPor")
SELECT a.slug,
       jsonb_build_object('mision', COALESCE(NULLIF(a.role, ''), a.name), 'tono', COALESCE(a.personality, ''), 'valores', '[]'::jsonb, 'limites', '[]'::jsonb, 'ejemplos', '[]'::jsonb),
       COALESCE(ar.name, a.area),
       CASE WHEN a.slug = 'orion' THEN NULL ELSE 'orion' END,
       CASE a.slug
         WHEN 'documentador'    THEN ARRAY['nota','consulta','redactar']
         WHEN 'booking-agent'   THEN ARRAY['consultar','crear','mover','editar','cancelar']
         WHEN 'backlog-manager' THEN ARRAY['consultar','crear_tarea','editar_tarea','crear_epica','editar_epica']
         WHEN 'presales'        THEN ARRAY['preparar_documento']
         ELSE ARRAY[]::text[]
       END,
       CASE WHEN a.slug IN ('documentador','booking-agent','backlog-manager','presales') THEN 'SUPERVISADA' ELSE 'ASISTIDA' END,
       'migracion-025'
FROM "Agent" a LEFT JOIN "Area" ar ON ar.id = a."areaId"
ON CONFLICT (slug) DO NOTHING;

-- Comunicación inicial de los 4 agentes con función real: atienden al equipo por Telegram y portal; Documentador y Presales colaboran.
INSERT INTO "AgenteComunicacion" ("agenteSlug", "tipoDestino", destino, direccion, canales, "requiereAprobacion", nota)
SELECT s, 'ROL', 'EQUIPO', 'AMBAS', ARRAY['TELEGRAM','PORTAL'], false, 'Atiende al equipo interno'
FROM unnest(ARRAY['documentador','booking-agent','backlog-manager','presales']) AS s
ON CONFLICT DO NOTHING;
INSERT INTO "AgenteComunicacion" ("agenteSlug", "tipoDestino", destino, direccion, canales, "requiereAprobacion", nota) VALUES
  ('documentador', 'AGENTE', 'presales', 'AMBAS', '{}', false, 'Redactan juntos los documentos de proyecto'),
  ('presales', 'AGENTE', 'documentador', 'AMBAS', '{}', false, 'Revisa el borrador del Documentador')
ON CONFLICT DO NOTHING;

-- Las tablas del portal pertenecen a portal_app (el rol con que corre portalhub). Si esta migración se aplica como postgres:
DO $$
DECLARE t text;
BEGIN
  IF EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'portal_app') THEN
    FOREACH t IN ARRAY ARRAY['AgenteFicha','AgenteFichaHistorial','AgenteComunicacion','GwPersona','GwIdentidad','GwHilo','GwMensaje','GwMemoria','GwDecision'] LOOP
      EXECUTE format('ALTER TABLE %I OWNER TO portal_app', t);
    END LOOP;
  END IF;
END $$;
