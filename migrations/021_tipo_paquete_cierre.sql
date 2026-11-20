-- MASD-0026-0002 · el paquete de cierre de fase es un tipo del catálogo (aditivo).
INSERT INTO "ArtefactoTipo" (clave, nombre, familia, descripcion, "faseDuena", entradas, productor, aprobador, visibilidad, destino, consumidores, vigencia, "entregableClaves", fuente, orden)
VALUES ('paquete_cierre','Paquete de cierre de fase','Especificación','Cómo cerró cada fase: criterios, actividades, artefactos, pendientes y estado del backlog. Lo genera el motor al avanzar la fase.',NULL,'[]','Motor (automático)','Responsable del proyecto','INTERNO','Hub (documento de la fase cerrada)','["sesiones","fase_siguiente"]','Se regenera cada vez que se cierra la fase','["paquete_cierre"]','{"tipo":"documento_fase"}',185)
ON CONFLICT (clave) DO NOTHING;
