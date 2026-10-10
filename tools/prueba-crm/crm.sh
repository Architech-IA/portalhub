#!/bin/bash
cd /root/portal-architechia; set -a; . ./.env; set +a
export PATH=$PATH:$(ls -d /root/.nvm/versions/node/*/bin | head -1)
node /root/crm.js "$@"
