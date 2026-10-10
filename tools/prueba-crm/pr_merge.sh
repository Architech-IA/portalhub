#!/bin/bash
# pr_merge.sh <rama> <título> <cuerpo>: sube la rama del sprint, abre el PR en GitHub y lo mergea a main (lo que haría la persona que revisa).
export PATH=$PATH:$(ls -d /root/.nvm/versions/node/*/bin | head -1)
set -a; . /root/portal-architechia/.env; set +a
REPO_DIR=/root/repos/prueba-crm-distribuidora-andina
API=https://api.github.com/repos/Architech-IA/prueba-crm-distribuidora-andina
RAMA="$1"; TITULO="$2"; CUERPO="$3"
cd "$REPO_DIR"
B64=$(printf 'x-access-token:%s' "$GITHUB_TOKEN" | base64 -w0)
git -c "http.https://github.com/.extraheader=AUTHORIZATION: basic $B64" push -q -u origin "$RAMA" --force 2>&1 | sed "s/$B64/***/g"
JSON=$(node -e "console.log(JSON.stringify({title:process.argv[1],body:process.argv[2],head:process.argv[3],base:'main'}))" "$TITULO" "$CUERPO" "$RAMA")
R=$(curl -s -X POST -H "Authorization: token $GITHUB_TOKEN" -H 'Accept: application/vnd.github+json' "$API/pulls" -d "$JSON")
N=$(echo "$R" | node -e "let s='';process.stdin.on('data',d=>s+=d).on('end',()=>{const j=JSON.parse(s);console.log(j.number||'');if(!j.number)console.error(s.slice(0,300))})")
echo "PR #$N"
M=$(curl -s -X PUT -H "Authorization: token $GITHUB_TOKEN" -H 'Accept: application/vnd.github+json' "$API/pulls/$N/merge" -d '{"merge_method":"merge"}')
echo "$M" | node -e "let s='';process.stdin.on('data',d=>s+=d).on('end',()=>{const j=JSON.parse(s);console.log('merge:', j.merged ? 'ok '+j.sha.slice(0,7) : s.slice(0,300))})"
