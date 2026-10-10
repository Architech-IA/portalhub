#!/bin/bash
# bg.sh <nombre> <args de crm.sh...>: corre en segundo plano (respetando las comillas) y deja la salida en /root/crm-<nombre>.log (termina con una línea FIN)
N="$1"; shift
ARGS=$(printf '%q ' "$@")
: > /root/crm-$N.log
( setsid nohup bash -c "/root/crm.sh $ARGS > /root/crm-$N.log 2>&1; echo FIN >> /root/crm-$N.log" > /dev/null 2>&1 < /dev/null & )
echo "lanzado $N"
