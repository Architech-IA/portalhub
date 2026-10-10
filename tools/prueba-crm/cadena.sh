#!/bin/bash
# cadena.sh <archivo json con taskIds> <nombre del log>: despacha la cadena de tareas por el Motor y deja el resultado en /root/crm-<nombre>.log
cd /root/portal-architechia; set -a; . ./.env; set +a
IDS=$(node -e "console.log(JSON.stringify(JSON.parse(require('fs').readFileSync('$1','utf8')).taskIds))" 2>/dev/null || /root/.nvm/versions/node/*/bin/node -e "console.log(JSON.stringify(JSON.parse(require('fs').readFileSync('$1','utf8')).taskIds))")
: > /root/crm-$2.log
( setsid nohup bash -c "curl -s -m 5400 -X POST http://127.0.0.1:3101/api/executor/dispatch-chain -H \"x-api-key: $INTERNAL_API_KEY\" -H 'content-type: application/json' -d '{\"taskIds\":$IDS}' > /root/crm-$2.log 2>&1; echo >> /root/crm-$2.log; echo FIN >> /root/crm-$2.log" > /dev/null 2>&1 < /dev/null & )
echo "cadena lanzada: $IDS" | cut -c1-120
