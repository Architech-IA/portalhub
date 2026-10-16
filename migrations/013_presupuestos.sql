-- Presupuestos de tokens por sesión («ses:<id>») y por proyecto («sol:<id>»). Sin fila = sin tope propio.
CREATE TABLE IF NOT EXISTS "ProyectoPresupuesto" (
  clave text PRIMARY KEY,
  tokens bigint NOT NULL,
  "actualizadoEn" timestamp(3) NOT NULL DEFAULT NOW()
);
