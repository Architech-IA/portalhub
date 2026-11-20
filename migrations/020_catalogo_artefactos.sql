-- MASD-0026-0001 · Catálogo de tipos de artefacto (guiado por datos). Migración ADITIVA: solo tablas nuevas.
-- Aplicar con psql (Prisma no conoce las tablas del motor):  psql "$DATABASE_URL" -f migrations/020_catalogo_artefactos.sql
-- Cada TIPO describe un documento del proyecto con su ficha: fase dueña, de qué deriva (entradas), quién lo produce y aprueba,
-- visibilidad, destino, quién lo consume y cuándo queda obsoleto. `entregableClaves` lo enlaza con los entregables de la plantilla
-- de fases y `fuente` dice dónde vive su contenido hoy (columna de Solucion, documento de fase, memoria...).

CREATE TABLE IF NOT EXISTS "ArtefactoTipo" (
  clave              text PRIMARY KEY,
  nombre             text NOT NULL,
  familia            text NOT NULL CHECK (familia IN ('Descubrimiento','Especificación','Entrega','Operación')),
  descripcion        text,
  "faseDuena"        text,
  entradas           jsonb NOT NULL DEFAULT '[]'::jsonb,
  productor          text,
  aprobador          text,
  visibilidad        text NOT NULL DEFAULT 'INTERNO' CHECK (visibilidad IN ('INTERNO','COMPARTIBLE')),
  destino            text,
  consumidores       jsonb NOT NULL DEFAULT '[]'::jsonb,
  vigencia           text,
  "entregableClaves" jsonb NOT NULL DEFAULT '[]'::jsonb,
  fuente             jsonb NOT NULL DEFAULT '{"tipo":"sin_soporte"}'::jsonb,
  activo             boolean NOT NULL DEFAULT true,
  orden              integer NOT NULL DEFAULT 0,
  "createdAt"        timestamp NOT NULL DEFAULT NOW(),
  "updatedAt"        timestamp NOT NULL DEFAULT NOW()
);

-- Con qué versión de cada entrada se produjo/revisó un artefacto. Si la entrada avanza, el artefacto queda DESACTUALIZADO.
CREATE TABLE IF NOT EXISTS "ArtefactoDerivacion" (
  "solucionId"    text NOT NULL,
  "claveHija"     text NOT NULL,
  "claveOrigen"   text NOT NULL,
  "versionOrigen" integer NOT NULL DEFAULT 0,
  "registradaEn"  timestamp NOT NULL DEFAULT NOW(),
  "registradaPor" text,
  PRIMARY KEY ("solucionId", "claveHija", "claveOrigen")
);
CREATE INDEX IF NOT EXISTS "ArtefactoDerivacion_sol_idx" ON "ArtefactoDerivacion" ("solucionId");

INSERT INTO "ArtefactoTipo" (clave, nombre, familia, descripcion, "faseDuena", entradas, productor, aprobador, visibilidad, destino, consumidores, vigencia, "entregableClaves", fuente, orden) VALUES
-- ───────── Descubrimiento ─────────
('ficha_cliente','Ficha del cliente','Descubrimiento','Datos esenciales del cliente y su contacto principal.','identificacion','[]','Agente de preventa y comercial','Responsable del proyecto','INTERNO','Hub (ficha del proyecto)','["briefing","propuesta_comercial","acta_diagnostico"]','Obsoleta si cambia el cliente o su contacto','["ficha_cliente","ficha_cliente_completa"]','{"tipo":"ficha"}',10),
('briefing','Briefing (primer contacto y reunión de diagnóstico)','Descubrimiento','Contexto del cliente, preguntas de descubrimiento y puntos a validar antes de reunirse.','contacto','["ficha_cliente"]','Agente de preventa','Responsable del proyecto','INTERNO','Hub (documento de fase)','["acta_diagnostico","cuestionario_alcance"]','Caduca cuando se realiza la reunión','["briefing_contacto","briefing_reunion"]','{"tipo":"documento_fase"}',20),
('cuestionario_alcance','Cuestionario de dimensionamiento','Descubrimiento','Preguntas estructuradas; cada respuesta alimenta un campo del PRD. Sin respuestas críticas la puerta no se abre.','diagnostico','["briefing","ficha_cliente"]','Agente de preventa y comercial','Responsable del proyecto','INTERNO','Hub','["prd","estimacion"]','Obsoleto si cambia el alcance','[]','{"tipo":"sin_soporte"}',30),
('acta_diagnostico','Acta del diagnóstico','Descubrimiento','Dolor y objetivos del cliente, decisiones, acciones y contradicciones con lo aprobado.','diagnostico','["briefing","cuestionario_alcance"]','Agente (acta)','Quien convocó la reunión','INTERNO','Hub y reuniones','["prd","propuesta_comercial"]','No caduca; se versiona','["acta","reunion_diagnostico"]','{"tipo":"documento_fase"}',40),
('estimacion','Estimación','Descubrimiento','Esfuerzo, plazos y costo estimados a partir del alcance.','propuesta','["cuestionario_alcance","acta_diagnostico"]','Agente de preventa','Responsable del proyecto','INTERNO','Hub (documento de fase)','["propuesta_comercial"]','Obsoleta si cambia el alcance','["estimacion","caso_negocio"]','{"tipo":"documento_fase"}',50),
('propuesta_comercial','Propuesta comercial','Descubrimiento','Oferta al cliente con alcance, plazos y condiciones.','propuesta','["acta_diagnostico","estimacion","ficha_cliente"]','Agente y comercial','Dirección','COMPARTIBLE','Hub (propuestas)','["prd"]','Vence con la validez de la oferta','["propuesta_enviada","propuesta_aceptada"]','{"tipo":"documento_fase"}',60),
('investigacion','Investigación','Descubrimiento','Pregunta, fuentes con fecha y confiabilidad, hallazgos y conclusión con nivel de confianza.',NULL,'[]','Agente y persona','Responsable del proyecto','INTERNO','Hub','["prd","disenoTecnico"]','Caduca en su fecha de revisión','[]','{"tipo":"sin_soporte"}',70),
-- ───────── Especificación ─────────
('prd','PRD','Especificación','Documento de requisitos del producto: problema, alcance, requisitos con prioridad, riesgos y métricas.','arranque','["acta_diagnostico","cuestionario_alcance","propuesta_comercial"]','Agente','Responsable del proyecto','INTERNO','Solución (PRD); en el futuro spec/prd.md','["requisitos","disenoTecnico","planEjecucion"]','Obsoleto si cambia el alcance','["prd_aprobado","alcance_base"]','{"tipo":"solucion_columna","columna":"prd"}',100),
('requisitos','Requisitos (REQ)','Especificación','Un requisito por funcionalidad con su criterio de aceptación.','arranque','["prd"]','Agente','Responsable del proyecto','INTERNO','PRD; en el futuro spec/requisitos/','["disenoTecnico","informe_pruebas","manual_usuario"]','Se versiona con el PRD','["requisitos"]','{"tipo":"prd_requisitos"}',110),
('disenoTecnico','Diseño técnico','Especificación','Entidades, stack, integraciones y decisiones técnicas.','diseno_plan','["prd","requisitos"]','Agente','Responsable del proyecto','INTERNO','Solución (diseño); en el futuro spec/diseno.md','["planEjecucion","informe_pruebas","runbook"]','Obsoleto si cambia el PRD','["diseno_aprobado","actualizar_diseno"]','{"tipo":"solucion_columna","columna":"disenoTecnico"}',120),
('arquitectura','Arquitectura (diagrama)','Especificación','Componentes y relaciones del sistema.','demo','["prd"]','Agente','Responsable del proyecto','INTERNO','Solución (arquitectura)','["disenoTecnico"]','Obsoleta si cambia el diseño','["esbozo_arquitectura"]','{"tipo":"solucion_columna","columna":"arquitectura"}',130),
('planEjecucion','Plan de ejecución','Especificación','Estrategia de QA, despliegue, RACI, gestión de cambios y comunicación.','diseno_plan','["prd","disenoTecnico"]','Agente','Responsable del proyecto','INTERNO','Solución (plan de ejecución)','["cronograma","planTrabajo"]','Obsoleto si cambia el diseño','["plan_aprobado","descomposicion_definida","backlog_creado"]','{"tipo":"solucion_columna","columna":"planEjecucion"}',140),
('cronograma','Cronograma','Especificación','Fechas de hitos y fases.','diseno_plan','["planEjecucion"]','Agente','Responsable del proyecto','INTERNO','Solución (cronograma)','[]','Obsoleto si cambia el plan','[]','{"tipo":"solucion_columna","columna":"cronograma"}',150),
('planTrabajo','Plan de trabajo','Especificación','Tareas y orden de trabajo.','diseno_plan','["planEjecucion"]','Agente','Responsable del proyecto','INTERNO','Solución (plan de trabajo)','[]','Obsoleto si cambia el plan','[]','{"tipo":"solucion_columna","columna":"planTrabajo"}',160),
('diagramas','Diagramas','Especificación','Diagramas de apoyo del proyecto.','diseno_plan','["disenoTecnico"]','Agente','Responsable del proyecto','INTERNO','Solución (diagramas)','[]','Obsoletos si cambia el diseño','[]','{"tipo":"solucion_columna","columna":"diagramas"}',170),
('memoria_proyecto','Estado vivo del proyecto','Especificación','Resumen corto y actual: dónde está, qué sigue, qué está bloqueado, riesgos abiertos.',NULL,'[]','Agente','Responsable del proyecto','INTERNO','Hub (memoria del proyecto)','["sesiones","paquete_cierre"]','Se actualiza al cerrar cada tarea y cada fase','[]','{"tipo":"memoria"}',180),
('decisiones','Registro de decisiones y supuestos','Especificación','Qué se decidió, por qué, quién lo aprobó y qué se descartó.',NULL,'[]','Agente y persona','Responsable del proyecto','INTERNO','Hub','["disenoTecnico","planEjecucion"]','No caduca; se versiona','[]','{"tipo":"sin_soporte"}',190),
-- ───────── Entrega ─────────
('informe_pruebas','Informe de QA','Entrega','Resultado de las pruebas contra los requisitos.','qa','["requisitos","disenoTecnico"]','Agente','Responsable del proyecto','INTERNO','Hub (documento de fase); en el futuro docs/','["acta_aceptacion","manual_usuario"]','Obsoleto si cambian los requisitos o el código','["informe_pruebas"]','{"tipo":"documento_fase"}',200),
('acta_aceptacion','Acta de aceptación','Entrega','Aceptación formal de la entrega por el cliente o el dueño.','qa','["informe_pruebas"]','Persona','Cliente o dueño','COMPARTIBLE','Hub (documento de fase)','["paquete_entrega"]','Vigente para la versión aceptada','["acta_aceptacion","uat"]','{"tipo":"documento_fase"}',210),
('runbook','Entorno y operación de producción','Entrega','Cómo se despliega, se monitorea y se recupera el sistema.','despliegue','["disenoTecnico"]','Agente','Responsable del proyecto','INTERNO','Hub; en el futuro docs/','["paquete_entrega","traspaso_operacion"]','Obsoleto si cambia el despliegue','["entorno_produccion","produccion","traspaso_operacion"]','{"tipo":"documento_fase"}',220),
('manual_usuario','Manual de usuario','Entrega','Guía de uso derivada de los requisitos y de la aplicación funcionando.','entrega_cierre','["requisitos","planEjecucion","informe_pruebas"]','Agente','Responsable del proyecto','COMPARTIBLE','Paquete de entrega; en el futuro docs/','["paquete_entrega"]','Obsoleto si cambia un requisito o una pantalla','["paquete_entrega","comunicacion_usuarios"]','{"tipo":"documento_fase"}',230),
-- ───────── Operación ─────────
('lecciones','Lecciones aprendidas','Operación','Qué funcionó, qué no y qué cambiar en plantillas y estimaciones.','entrega_cierre','["memoria_proyecto"]','Agente','Responsable del proyecto','INTERNO','Hub (documento de fase)','["estimacion"]','No caduca','["lecciones","medicion"]','{"tipo":"documento_fase"}',300),
('incidente','Incidente','Operación','Fallo en producción: impacto, causa y corrección. Se convierte en requisito y prueba.',NULL,'["manual_usuario"]','Agente y persona','Responsable del proyecto','INTERNO','Hub','["requisitos","informe_pruebas"]','Se cierra al publicar la corrección','[]','{"tipo":"sin_soporte"}',310),
('solicitud_cambio','Solicitud de cambio','Operación','Cambio pedido con el sistema en uso: ciclo corto de especificación, prueba y construcción.',NULL,'["prd"]','Persona','Responsable del proyecto','INTERNO','Hub','["prd","requisitos"]','Se cierra al publicar la versión','[]','{"tipo":"sin_soporte"}',320),
('informe_estabilizacion','Informe de estabilización','Operación','Resultados del periodo de hipercuidado y criterios de salida.',NULL,'["incidente"]','Agente','Responsable del proyecto','COMPARTIBLE','Hub','[]','Vigente para la versión estabilizada','[]','{"tipo":"sin_soporte"}',330)
ON CONFLICT (clave) DO NOTHING;
