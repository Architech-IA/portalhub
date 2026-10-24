-- Tipos de propuesta de la IA para los que el proyecto dejó de pedir confirmación (solo los de bajo riesgo: ficha, notas, tareas).
CREATE TABLE IF NOT EXISTS "ProyectoAutoAccion" (
  "solucionId" text NOT NULL,
  tipo text NOT NULL,
  "porNombre" text,
  "createdAt" timestamptz NOT NULL DEFAULT NOW(),
  PRIMARY KEY ("solucionId", tipo)
);
