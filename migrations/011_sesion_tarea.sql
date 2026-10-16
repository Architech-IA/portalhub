-- Tareas del backlog que una sesión de desarrollo tomó y (si lo hizo) cerró.
CREATE TABLE IF NOT EXISTS "ProyectoSesionTarea" (
  id text PRIMARY KEY,
  "sesionId" text NOT NULL,
  "tareaId" text NOT NULL,
  "solucionId" text NOT NULL REFERENCES "Solucion"(id) ON DELETE CASCADE,
  estado text NOT NULL DEFAULT 'EN_CURSO',
  "tomadaEn" timestamp(3) NOT NULL DEFAULT NOW(),
  "cerradaEn" timestamp(3),
  UNIQUE ("sesionId", "tareaId")
);
