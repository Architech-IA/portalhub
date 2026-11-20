#!/bin/bash
# Publica la biblioteca del repo (biblioteca/) en /opt/portalhub/biblioteca, donde la leen el servicio público (usuario sin
# privilegios, que no ve /root) y el motor. Editar la biblioteca NO exige recompilar: cambia los archivos, haz commit y ejecuta esto.
set -euo pipefail
cd "$(dirname "$0")/.."
DEST=/opt/portalhub/biblioteca
rm -rf "$DEST.nuevo"; mkdir -p "$DEST.nuevo"
cp -a biblioteca/. "$DEST.nuevo/"
chmod -R a+rX "$DEST.nuevo"
rm -rf "$DEST.anterior"; [ -d "$DEST" ] && mv "$DEST" "$DEST.anterior"
mv "$DEST.nuevo" "$DEST"
echo "biblioteca publicada en $DEST ($(find "$DEST" -type f | wc -l) archivos)"
