# Prueba del CRM a la medida (idea → MVP → proyecto → entrega)

Recorrido de punta a punta del Motor Agéntico con un lead ficticio. Resultados y lecciones: `docs/prueba-crm.md`.

Los scripts corren **en la VPS** (usan el `.env` del portal, Prisma, el servicio del Motor en `:3101` y Chromium de Playwright). Los identificadores del proyecto se leen de `/root/crm_ids.json` (`{"leadId":…,"solucionId":…}`), que crea `paso1_lead.js` a mano tras el primer paso.

| Archivo | Para qué |
|---|---|
| `paso1_lead.js` | Crea el lead como «Nuevo lead» (POST /api/leads) |
| `crm.js` / `crm.sh` | Opera el proyecto: `estado`, `marcar`, `avanzar`, `ganado`, `correr "<título>"`, `propuesta`, `adjuntar`, `tareas` |
| `bg.sh`, `cadena.sh`, `motor.sh` | Correr en segundo plano, despachar una cadena de tareas, llamar al servicio del Motor |
| `notas_hub.js` | Notas del Lead Hub (reunión de diagnóstico simulada) |
| `scaffold.sh` | Crea el repositorio privado y arma la base (Next.js, Prisma, migración inicial) |
| `hub_mvp.js` | PRD corto, diseño técnico, diagrama y sprint «MVP» (8 tareas encadenadas) |
| `schema2.sh`, `hub_endurecimiento.js`, `plan_ejec.js` | Esquema con usuarios, PRD v2, plan de ejecución y sprint de endurecimiento |
| `e2e_crm.js`, `e2e_crm2.js` | Pruebas de aceptación en navegador (MVP; usuarios, roles, ataque, celular) |
| `pr_merge.sh`, `merge_pr.sh` | Subir la rama del sprint y mergear el PR (la revisión humana) |

Orden: lead → `crm.js marcar/avanzar` por fase → `scaffold.sh` → `hub_mvp.js` → `cadena.sh` → PR → base de datos y despliegue por el Motor → QA.
