-- Una sesión de IA puede nacer de una actividad de la fase (botón «Iniciar con IA»): al cerrarla, la actividad queda hecha.
ALTER TABLE "ProyectoSesion" ADD COLUMN IF NOT EXISTS "actividadClave" text;
