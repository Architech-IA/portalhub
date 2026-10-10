#!/bin/bash
# merge_pr.sh <número> <rama>: asegura que la rama del sprint esté subida y mergea el PR (revisión humana ya hecha).
export PATH=$PATH:$(ls -d /root/.nvm/versions/node/*/bin | head -1)
set -a; . /root/portal-architechia/.env; set +a
API=https://api.github.com/repos/Architech-IA/prueba-crm-distribuidora-andina
cd /root/repos/prueba-crm-distribuidora-andina
B64=$(printf 'x-access-token:%s' "$GITHUB_TOKEN" | base64 -w0)
git -c "http.https://github.com/.extraheader=AUTHORIZATION: basic $B64" push -q origin "$2" --force 2>&1 | sed "s/$B64/***/g"
echo "rama local: $(git rev-parse --short $2)"
for i in 1 2 3; do
  H=$(curl -s -H "Authorization: token $GITHUB_TOKEN" $API/pulls/$1 | node -e "let s='';process.stdin.on('data',d=>s+=d).on('end',()=>{const p=JSON.parse(s);console.log(p.head.sha.slice(0,7))})")
  [ "$H" = "$(git rev-parse --short $2)" ] && break; sleep 3
done
echo "PR #$1 head: $H"
M=$(curl -s -X PUT -H "Authorization: token $GITHUB_TOKEN" -H 'Accept: application/vnd.github+json' "$API/pulls/$1/merge" -d '{"merge_method":"merge"}')
echo "$M" | node -e "let s='';process.stdin.on('data',d=>s+=d).on('end',()=>{const j=JSON.parse(s);console.log('merge:', j.merged ? 'ok '+j.sha.slice(0,7) : s.slice(0,300))})"
