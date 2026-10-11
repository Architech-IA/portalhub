-- Espacio de trabajo unificado (MASD-0024-0013): toda pieza de la solución (PRD, diseño, plan, arquitectura, cronograma, diagramas)
-- se edita con el mismo modelo: estado, versiones, comentarios y propuestas del motor. Aditivo y repetible; solo lo usa portalhub.

-- Estado de los artefactos que NO guardan su propio estado dentro del documento (arquitectura, cronograma, diagramas, plan de trabajo).
CREATE TABLE IF NOT EXISTS "ArtefactoEstado" (
  "solucionId"      text NOT NULL REFERENCES "Solucion"(id) ON DELETE CASCADE,
  clave             text NOT NULL,
  estado            text NOT NULL DEFAULT 'BORRADOR',      -- BORRADOR | EN_REVISION | APROBADO
  "aprobadoPor"     text,
  "aprobadoEn"      timestamp(3),
  "reabiertoEn"     timestamp(3),
  "updatedAt"       timestamp(3) NOT NULL DEFAULT NOW(),
  PRIMARY KEY ("solucionId", clave)
);

-- Versiones de todos los artefactos menos el PRD (que usa SolucionPrdVersion): la anterior de cada edición guardada.
CREATE TABLE IF NOT EXISTS "ArtefactoVersion" (
  id                text PRIMARY KEY,
  "solucionId"      text NOT NULL REFERENCES "Solucion"(id) ON DELETE CASCADE,
  clave             text NOT NULL,
  version           integer NOT NULL,
  contenido         text NOT NULL,
  motivo            text,
  "usuarioNombre"   text,
  "createdAt"       timestamp(3) NOT NULL DEFAULT NOW(),
  UNIQUE ("solucionId", clave, version)
);

CREATE TABLE IF NOT EXISTS "ArtefactoComentario" (
  id                text PRIMARY KEY,
  "solucionId"      text NOT NULL REFERENCES "Solucion"(id) ON DELETE CASCADE,
  clave             text NOT NULL,
  "autorId"         text,
  "autorNombre"     text NOT NULL,
  texto             text NOT NULL,
  resuelto          boolean NOT NULL DEFAULT false,
  "createdAt"       timestamp(3) NOT NULL DEFAULT NOW()
);
CREATE INDEX IF NOT EXISTS "ArtefactoComentario_sol_idx" ON "ArtefactoComentario" ("solucionId", clave, "createdAt");

-- Lo que el motor (o una persona) propone cambiar en un artefacto: no se aplica hasta que alguien lo acepta.
CREATE TABLE IF NOT EXISTS "ArtefactoPropuesta" (
  id                text PRIMARY KEY,
  "solucionId"      text NOT NULL REFERENCES "Solucion"(id) ON DELETE CASCADE,
  clave             text NOT NULL,
  titulo            text NOT NULL,
  detalle           text,
  ops               jsonb NOT NULL DEFAULT '[]'::jsonb,   -- [{ op: agregar|desplazar, ruta, valor | dias, desdeId }]
  origen            text NOT NULL DEFAULT 'PERSONA',      -- TAREA | REVISOR | PERSONA
  "tareaId"         text,
  estado            text NOT NULL DEFAULT 'PENDIENTE',    -- PENDIENTE | ACEPTADA | RECHAZADA
  "creadoPor"       text,
  "decididoPor"     text,
  "decididoEn"      timestamp(3),
  "decisionNota"    text,
  "createdAt"       timestamp(3) NOT NULL DEFAULT NOW()
);
CREATE INDEX IF NOT EXISTS "ArtefactoPropuesta_sol_idx" ON "ArtefactoPropuesta" ("solucionId", estado, "createdAt" DESC);
