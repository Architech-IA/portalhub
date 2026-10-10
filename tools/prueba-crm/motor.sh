#!/bin/bash
# motor.sh <ruta> [json]: llama al servicio privilegiado del Motor con la clave interna (POST). Ej: motor.sh /api/proyectos/ID/db
cd /root/portal-architechia; set -a; . ./.env; set +a
curl -s -m 1500 -w '\n[HTTP %{http_code}]\n' -X POST "http://127.0.0.1:3101$1" -H "x-api-key: $INTERNAL_API_KEY" -H 'content-type: application/json' -d "${2:-{\}}"
