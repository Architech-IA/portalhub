-- MASD-0026-0009 y MASD-0026-0010 · autonomía por fase y sistema de diseño por proyecto. Migración ADITIVA (solo tablas nuevas).
-- Aplicar con psql:  psql "$DATABASE_URL" -f migrations/022_autonomia_diseno.sql

-- Nivel de autonomía de una fase de un proyecto. Sin fila = nivel por defecto (ver routes/metricas.rs::nivel_por_defecto).
CREATE TABLE IF NOT EXISTS "ProyectoAutonomia" (
  "solucionId"     text NOT NULL,
  "faseClave"      text NOT NULL,
  nivel            text NOT NULL CHECK (nivel IN ('ASISTIDA','SUPERVISADA','AUTONOMA')),
  "actualizadoPor" text,
  "updatedAt"      timestamp NOT NULL DEFAULT NOW(),
  PRIMARY KEY ("solucionId", "faseClave")
);

-- Sistema de diseño elegido para un proyecto (nombre de una carpeta de biblioteca/sistemas-diseno).
CREATE TABLE IF NOT EXISTS "ProyectoSistemaDiseno" (
  "solucionId"     text PRIMARY KEY,
  sistema          text NOT NULL,
  "actualizadoPor" text,
  "updatedAt"      timestamp NOT NULL DEFAULT NOW()
);
