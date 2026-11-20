-- 026: lo que el portal leía de src/lib/orquestador/registro.ts (código) pasa a la ficha del agente (datos).
-- Se aplica como portal_app (dueño de las tablas): psql -U portal_app o `sudo -u postgres psql` con SET ROLE portal_app.
ALTER TABLE "AgenteFicha"
  ADD COLUMN IF NOT EXISTS funciones text[] NOT NULL DEFAULT '{}',          -- qué hace, en lenguaje de la persona
  ADD COLUMN IF NOT EXISTS canales   text[] NOT NULL DEFAULT '{}',          -- por dónde se usa (etiquetas para mostrar)
  ADD COLUMN IF NOT EXISTS bot       text,                                  -- usuario del bot de Telegram, si tiene
  ADD COLUMN IF NOT EXISTS "etiquetasAcciones" jsonb NOT NULL DEFAULT '{}'::jsonb; -- {clave: etiqueta legible}
-- La siembra de los 4 agentes con función real la genera sembrar_registro.mjs desde registro.ts (una sola vez).
