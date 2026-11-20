# Copia a ~/.claude/skills las skills de la biblioteca marcadas con "sincronizarClaudeCode": true y que traen SKILL.md.
# Nunca pisa una skill que ya existe, salvo que pases -Forzar. Uso: .\sincronizar.ps1 [-Forzar]
param([switch]$Forzar)
$ErrorActionPreference = 'Stop'
$aqui = Split-Path -Parent $MyInvocation.MyCommand.Path
$destino = if ($env:CLAUDE_SKILLS_DIR) { $env:CLAUDE_SKILLS_DIR } else { Join-Path $HOME '.claude\skills' }
New-Item -ItemType Directory -Force -Path $destino | Out-Null
$n = 0
foreach ($ficha in Get-ChildItem -Path (Join-Path $aqui 'skills') -Recurse -Filter 'FICHA.json') {
  $dir = $ficha.DirectoryName; $nombre = Split-Path -Leaf $dir
  $json = Get-Content $ficha.FullName -Raw | ConvertFrom-Json
  if (-not $json.sincronizarClaudeCode -or -not (Test-Path (Join-Path $dir 'SKILL.md'))) { Write-Host "omitida: $nombre (no está marcada o no trae SKILL.md)"; continue }
  $dest = Join-Path $destino $nombre
  if ((Test-Path $dest) -and -not $Forzar) { Write-Host "ya existe, no se toca: $nombre (usa -Forzar para reemplazar)"; continue }
  if (Test-Path $dest) { Remove-Item -Recurse -Force $dest }
  New-Item -ItemType Directory -Force -Path $dest | Out-Null
  Copy-Item (Join-Path $dir 'SKILL.md') (Join-Path $dest 'SKILL.md')
  if (Test-Path (Join-Path $dir 'referencias')) { Copy-Item -Recurse (Join-Path $dir 'referencias') (Join-Path $dest 'reference') }
  Write-Host "sincronizada: $nombre"; $n++
}
Write-Host "listo: $n skill(s) sincronizada(s) en $destino"
