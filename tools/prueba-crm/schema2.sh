#!/bin/bash
# Cambio de esquema que hace el arquitecto (una persona) antes del sprint de endurecimiento: Usuario con rol y Actividad.responsable.
set -e
export PATH=$PATH:$(ls -d /root/.nvm/versions/node/*/bin | head -1)
set -a; . /root/portal-architechia/.env; set +a
REPO=/root/repos/prueba-crm-distribuidora-andina
cd "$REPO"
B64=$(printf 'x-access-token:%s' "$GITHUB_TOKEN" | base64 -w0)
GH="git -c http.https://github.com/.extraheader=AUTHORIZATION:\ basic\ $B64"
git -c "http.https://github.com/.extraheader=AUTHORIZATION: basic $B64" fetch -q origin main
git checkout -q main
git reset -q --hard origin/main
git log --oneline | head -2

cp prisma/schema.prisma /tmp/schema_antes.prisma
cat >> prisma/schema.prisma <<'EOF'

enum RolUsuario {
  VENDEDOR
  JEFATURA
}

model Usuario {
  id        String     @id @default(cuid())
  nombre    String
  email     String     @unique
  rol       RolUsuario @default(VENDEDOR)
  claveHash String
  creadoEn  DateTime   @default(now())
}
EOF
# Actividad.responsable (email del vendedor a quien pertenece la actividad)
node -e "
const fs=require('fs');let t=fs.readFileSync('prisma/schema.prisma','utf8');
t=t.replace('  hecha         Boolean      @default(false)\n','  hecha         Boolean      @default(false)\n  responsable   String?\n');
fs.writeFileSync('prisma/schema.prisma',t)"
grep -n "responsable" prisma/schema.prisma
mkdir -p prisma/migrations/0002_usuarios
npx prisma migrate diff --from-schema-datamodel /tmp/schema_antes.prisma --to-schema-datamodel prisma/schema.prisma --script > prisma/migrations/0002_usuarios/migration.sql
cat prisma/migrations/0002_usuarios/migration.sql
npx prisma validate 2>&1 | tail -1
npx prisma generate 2>&1 | grep -i "generated" | head -1
npx tsc --noEmit && echo "tsc OK"

git add -A
git -c user.name="Claude" -c user.email="noreply@anthropic.com" commit -q -m "Esquema: Usuario con rol y Actividad.responsable (migración 0002) — base del sprint de endurecimiento"
git -c "http.https://github.com/.extraheader=AUTHORIZATION: basic $B64" push -q origin HEAD:main 2>&1 | sed "s/$B64/***/g"
git log --oneline | head -2
