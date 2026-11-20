#!/usr/bin/env bash
# Copia a ~/.claude/skills las skills de la biblioteca marcadas con "sincronizarClaudeCode": true y que traen SKILL.md.
# Nunca pisa una skill que ya existe, salvo que pases --forzar. Uso: ./sincronizar.sh [--forzar]
set -euo pipefail
AQUI="$(cd "$(dirname "$0")" && pwd)"
DESTINO="${CLAUDE_SKILLS_DIR:-$HOME/.claude/skills}"
FORZAR="${1:-}"
mkdir -p "$DESTINO"
n=0
for ficha in "$AQUI"/skills/*/FICHA.json; do
  [ -e "$ficha" ] || continue
  dir="$(dirname "$ficha")"; nombre="$(basename "$dir")"
  marcada="$(grep -c '"sincronizarClaudeCode": *true' "$ficha" || true)"
  if [ "$marcada" = "0" ] || [ ! -f "$dir/SKILL.md" ]; then echo "omitida: $nombre (no está marcada o no trae SKILL.md)"; continue; fi
  if [ -e "$DESTINO/$nombre" ] && [ "$FORZAR" != "--forzar" ]; then echo "ya existe, no se toca: $nombre (usa --forzar para reemplazar)"; continue; fi
  rm -rf "$DESTINO/$nombre"; mkdir -p "$DESTINO/$nombre"
  cp "$dir/SKILL.md" "$DESTINO/$nombre/SKILL.md"
  [ -d "$dir/referencias" ] && cp -r "$dir/referencias" "$DESTINO/$nombre/reference"
  echo "sincronizada: $nombre"; n=$((n+1))
done
echo "listo: $n skill(s) sincronizada(s) en $DESTINO"
