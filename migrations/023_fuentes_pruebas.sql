-- MASD-0026-0005 (fuentes, actas, investigación) y MASD-0026-0004 (pruebas de aceptación). Migración ADITIVA (solo tablas nuevas).
-- Aplicar con psql:  psql "$DATABASE_URL" -f migrations/023_fuentes_pruebas.sql

-- Evidencia del proyecto: audios (con su transcripción), notas, actas, documentos, correos e investigaciones. No se edita: se procesa.
CREATE TABLE IF NOT EXISTS "FuenteProyecto" (
  id               text PRIMARY KEY,
  "solucionId"     text NOT NULL,
  tipo             text NOT NULL CHECK (tipo IN ('AUDIO','NOTA','ACTA','DOCUMENTO','CORREO','INVESTIGACION')),
  titulo           text NOT NULL,
  contenido        text NOT NULL DEFAULT '',
  fecha            timestamp,
  "faseClave"      text,
  visibilidad      text NOT NULL DEFAULT 'INTERNO' CHECK (visibilidad IN ('INTERNO','COMPARTIBLE')),
  origen           text,
  enlace           text,
  meta             jsonb NOT NULL DEFAULT '{}'::jsonb,
  estado           text NOT NULL DEFAULT 'SIN_PROCESAR' CHECK (estado IN ('SIN_PROCESAR','PROCESADA','ERROR')),
  resumen          text,
  "creadoPor"      text,
  "createdAt"      timestamp NOT NULL DEFAULT NOW(),
  "updatedAt"      timestamp NOT NULL DEFAULT NOW()
);
CREATE INDEX IF NOT EXISTS "FuenteProyecto_sol_idx" ON "FuenteProyecto" ("solucionId", "createdAt" DESC);

-- Lo que se concluye de una fuente: decisiones, acciones, preguntas, riesgos y contradicciones. Cada ítem espera una decisión humana.
CREATE TABLE IF NOT EXISTS "ActaItem" (
  id                  text PRIMARY KEY,
  "fuenteId"          text NOT NULL REFERENCES "FuenteProyecto"(id) ON DELETE CASCADE,
  "solucionId"        text NOT NULL,
  tipo                text NOT NULL CHECK (tipo IN ('DECISION','ACCION','PREGUNTA','RIESGO','CONTRADICCION')),
  texto               text NOT NULL,
  cita                text,
  "destinoArtefacto"  text,
  responsable         text,
  estado              text NOT NULL DEFAULT 'PENDIENTE' CHECK (estado IN ('PENDIENTE','ACEPTADO','DESCARTADO')),
  resultado           text,
  "decididoPor"       text,
  "decididoEn"        timestamp,
  "createdAt"         timestamp NOT NULL DEFAULT NOW()
);
CREATE INDEX IF NOT EXISTS "ActaItem_sol_idx" ON "ActaItem" ("solucionId", estado);
CREATE INDEX IF NOT EXISTS "ActaItem_fuente_idx" ON "ActaItem" ("fuenteId");

-- Pruebas de aceptación derivadas de los requisitos del PRD. Las propone la IA y las aprueba una persona antes de construir.
CREATE TABLE IF NOT EXISTS "PruebaAceptacion" (
  id             text PRIMARY KEY,
  "solucionId"   text NOT NULL,
  "reqCodigo"    text NOT NULL,
  "reqId"        text,
  titulo         text NOT NULL,
  escenarios     text NOT NULL,
  estado         text NOT NULL DEFAULT 'PROPUESTA' CHECK (estado IN ('PROPUESTA','APROBADA','DESCARTADA')),
  origen         text NOT NULL DEFAULT 'IA',
  "creadoPor"    text,
  "decididoPor"  text,
  "decididoEn"   timestamp,
  "createdAt"    timestamp NOT NULL DEFAULT NOW(),
  "updatedAt"    timestamp NOT NULL DEFAULT NOW()
);
CREATE INDEX IF NOT EXISTS "PruebaAceptacion_sol_idx" ON "PruebaAceptacion" ("solucionId", "reqCodigo");

-- Tipos nuevos del catálogo de artefactos (los que antes figuraban SIN_SOPORTE).
UPDATE "ArtefactoTipo" SET fuente = '{"tipo":"fuentes","tiposFuente":["INVESTIGACION"]}'::jsonb, "updatedAt" = NOW() WHERE clave = 'investigacion';
INSERT INTO "ArtefactoTipo" (clave, nombre, familia, descripcion, "faseDuena", entradas, productor, aprobador, visibilidad, destino, consumidores, vigencia, "entregableClaves", fuente, orden)
VALUES
('acta_reunion','Acta, nota o transcripción','Descubrimiento','Evidencia de una reunión, llamada o conversación (grabada o no): se procesa en decisiones, acciones, preguntas y riesgos.',NULL,'[]','Agente (extracción) y persona','Quien convocó','INTERNO','Hub (Fuentes)','["prd","disenoTecnico","planEjecucion","estado_vivo"]','No caduca; cada ítem se decide una vez','[]','{"tipo":"fuentes","tiposFuente":["AUDIO","NOTA","ACTA","CORREO","DOCUMENTO"]}',45),
('pruebas_aceptacion','Pruebas de aceptación','Especificación','Escenarios (dado, cuando, entonces) derivados del criterio de aceptación de cada requisito, aprobados por una persona.','diseno_plan','["requisitos"]','Agente (propuesta)','Responsable del proyecto','INTERNO','Hub; en el futuro spec/pruebas/','["informe_pruebas","manual_usuario"]','Obsoletas si cambia el requisito','[]','{"tipo":"pruebas_aceptacion"}',115)
ON CONFLICT (clave) DO NOTHING;
