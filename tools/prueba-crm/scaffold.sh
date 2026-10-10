#!/bin/bash
# Base del proyecto CRM (la hace una persona: los agentes no escriben package.json ni lockfiles). Se corre UNA vez en la VPS.
set -e
export PATH=$PATH:$(ls -d /root/.nvm/versions/node/*/bin | head -1)
set -a; . /root/portal-architechia/.env; set +a
SOL=$(sed -E 's/.*"solucionId":"([^"]+)".*/\1/' /root/crm_ids.json)

echo "== 1. crear el repositorio privado desde el Motor"
R=$(curl -s -m 120 -X POST http://127.0.0.1:3101/internal/crear-repositorio -H "x-api-key: $INTERNAL_API_KEY" -H 'content-type: application/json' \
  -d "{\"solucionId\":\"$SOL\",\"nombre\":\"prueba-crm-distribuidora-andina\",\"privado\":true}")
echo "$R"
REPO=/root/repos/prueba-crm-distribuidora-andina
[ -d "$REPO/.git" ] || { echo "el repositorio no quedó clonado"; exit 1; }
cd "$REPO"
git checkout -q main 2>/dev/null || git checkout -q -b main

echo "== 2. archivos base"
mkdir -p src/app src/lib prisma tests
cat > package.json <<'EOF'
{
  "name": "crm-distribuidora-andina",
  "version": "0.1.0",
  "private": true,
  "scripts": {
    "dev": "next dev",
    "build": "prisma generate && next build",
    "start": "next start",
    "typecheck": "tsc --noEmit",
    "test": "tsx --test tests/*.test.ts"
  },
  "dependencies": {
    "@prisma/client": "5.22.0",
    "next": "14.2.15",
    "react": "18.3.1",
    "react-dom": "18.3.1"
  },
  "devDependencies": {
    "@types/node": "20.16.11",
    "@types/react": "18.3.11",
    "@types/react-dom": "18.3.1",
    "prisma": "5.22.0",
    "tsx": "4.19.1",
    "typescript": "5.6.3"
  }
}
EOF
cat > tsconfig.json <<'EOF'
{
  "compilerOptions": {
    "target": "ES2022", "lib": ["dom", "dom.iterable", "esnext"], "allowJs": false, "skipLibCheck": true, "strict": true, "noEmit": true,
    "esModuleInterop": true, "module": "esnext", "moduleResolution": "bundler", "resolveJsonModule": true, "isolatedModules": true, "jsx": "preserve",
    "incremental": true, "plugins": [{ "name": "next" }], "paths": { "@/*": ["./src/*"] }
  },
  "include": ["next-env.d.ts", "**/*.ts", "**/*.tsx", ".next/types/**/*.ts"],
  "exclude": ["node_modules"]
}
EOF
cat > next.config.js <<'EOF'
/** @type {import('next').NextConfig} */
module.exports = { output: 'standalone' }
EOF
cat > next-env.d.ts <<'EOF'
/// <reference types="next" />
/// <reference types="next/image-types/global" />
EOF
cat > .gitignore <<'EOF'
node_modules
.next
.env*
*.tsbuildinfo
EOF
cat > prisma/schema.prisma <<'EOF'
// Modelo de datos del CRM — sale del Diseño técnico del hub (Empresa, Contacto, Oportunidad, Actividad). Usuario/roles llegan en la fase de endurecimiento.
generator client {
  provider = "prisma-client-js"
}

datasource db {
  provider = "postgresql"
  url      = env("DATABASE_URL")
}

enum EtapaOportunidad {
  PROSPECTO
  CONTACTADO
  COTIZADO
  NEGOCIACION
  GANADA
  PERDIDA
}

enum TipoActividad {
  LLAMADA
  VISITA
  CORREO
  TAREA
}

model Empresa {
  id            String        @id @default(cuid())
  nit           String        @unique
  razonSocial   String
  ciudad        String?
  sector        String?
  vendedor      String?
  creadaEn      DateTime      @default(now())
  contactos     Contacto[]
  oportunidades Oportunidad[]
}

model Contacto {
  id          String      @id @default(cuid())
  nombre      String
  cargo       String?
  telefono    String?
  email       String?
  empresaId   String
  empresa     Empresa     @relation(fields: [empresaId], references: [id], onDelete: Cascade)
  creadoEn    DateTime    @default(now())
  actividades Actividad[]
}

model Oportunidad {
  id             String           @id @default(cuid())
  titulo         String
  valor          Float            @default(0)
  etapa          EtapaOportunidad @default(PROSPECTO)
  cierreEstimado DateTime?
  vendedor       String?
  empresaId      String
  empresa        Empresa          @relation(fields: [empresaId], references: [id], onDelete: Cascade)
  creadaEn       DateTime         @default(now())
  actividades    Actividad[]
}

model Actividad {
  id            String       @id @default(cuid())
  tipo          TipoActividad
  descripcion   String
  fecha         DateTime
  recordatorio  DateTime?
  hecha         Boolean      @default(false)
  oportunidadId String?
  oportunidad   Oportunidad? @relation(fields: [oportunidadId], references: [id], onDelete: SetNull)
  contactoId    String?
  contacto      Contacto?    @relation(fields: [contactoId], references: [id], onDelete: SetNull)
  creadaEn      DateTime     @default(now())
}
EOF
cat > src/lib/db.ts <<'EOF'
import { PrismaClient } from '@prisma/client'

// Un solo cliente de Prisma por proceso (en desarrollo, Next recarga módulos y abriría conexiones de más).
const globalPrisma = globalThis as unknown as { prisma?: PrismaClient }
export const prisma = globalPrisma.prisma ?? new PrismaClient()
if (process.env.NODE_ENV !== 'production') globalPrisma.prisma = prisma
EOF
cat > src/app/layout.tsx <<'EOF'
import type { ReactNode } from 'react'

export const metadata = { title: 'CRM Distribuidora Andina' }

export default function RootLayout({ children }: { children: ReactNode }) {
  return (
    <html lang="es">
      <body style={{ fontFamily: 'system-ui, sans-serif', margin: 0, background: '#f7f7f8', color: '#111' }}>{children}</body>
    </html>
  )
}
EOF
cat > src/app/page.tsx <<'EOF'
export default function Inicio() {
  return (
    <main style={{ padding: 24 }}>
      <h1>CRM Distribuidora Andina</h1>
      <p>Base del proyecto. Las pantallas se construyen por tareas del Motor.</p>
    </main>
  )
}
EOF
cat > README.md <<'EOF'
# CRM Distribuidora Andina

CRM a la medida: empresas, contactos, oportunidades (tablero por etapas), actividades con recordatorios y tablero de KPIs.
Proyecto de prueba del recorrido idea → MVP → proyecto → entrega del Motor Agéntico de ArchitechIA.

- Next.js 14 (App Router) + TypeScript + Prisma + PostgreSQL.
- `npm run typecheck`, `npm test`, `npm run build`.
- La variable `DATABASE_URL` la provee el despliegue.
EOF

echo "== 3. dependencias, cliente de Prisma y migración inicial"
npm install --no-audit --no-fund --loglevel=error 2>&1 | tail -3
npx prisma generate 2>&1 | tail -2
mkdir -p prisma/migrations/0001_init
printf 'provider = "postgresql"\n' > prisma/migrations/migration_lock.toml
npx prisma migrate diff --from-empty --to-schema-datamodel prisma/schema.prisma --script > prisma/migrations/0001_init/migration.sql
wc -l prisma/migrations/0001_init/migration.sql
echo "== 4. comprobación: tsc"
npx tsc --noEmit && echo "tsc OK"

echo "== 5. commit y push a main"
cat > /tmp/askpass_crm.sh <<'EOT'
#!/bin/sh
case "$1" in *sername*) echo x-access-token;; *) echo "$GITHUB_TOKEN";; esac
EOT
chmod 700 /tmp/askpass_crm.sh
git add -A
git -c user.name="Claude" -c user.email="noreply@anthropic.com" commit -q -m "Base del proyecto: Next.js, Prisma, esquema inicial y migración 0001" || true
GIT_ASKPASS=/tmp/askpass_crm.sh GIT_TERMINAL_PROMPT=0 git push -q origin HEAD:main 2>&1 | sed 's/ghp_[A-Za-z0-9]*/***/g'
rm -f /tmp/askpass_crm.sh
git log --oneline | head -3
