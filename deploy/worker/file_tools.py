"""
file_tools.py — Herramientas de archivo seguras para agentes CODE del
Motor Agentico SDD. Da acceso real de lectura/escritura al repo del
portal, con guardrails:

- Lectura (read_file): cualquier archivo dentro del repo, salvo secretos
  (.env*, .git/, .github/, ecosistema de pm2, lockfiles). Se puede leer por
  rangos de lineas (start_line/end_line) y leer package.json y el esquema de
  Prisma. grep_files busca en codigo y configuracion.
- Escritura (write_file / edit_file / delete_file / move_file): solo en las
  rutas de WRITE_ALLOWED_PREFIXES/WRITE_ALLOWED_FILES (src/, app/, tests/,
  docs/, prisma/migrations/, README.md, Dockerfile...), ampliables por repo con
  .masd-policy.json (escrito por una persona; el agente no puede tocarlo).
  Nunca .env, .git, .github ni node_modules, ni nada fuera del repo (sin path
  traversal). package.json y el esquema de Prisma no se escriben salvo que la
  politica del repo lo autorice. edit_file cambia un trozo EXACTO sin reescribir
  el archivo; write_file rechaza reescrituras truncadas de archivos grandes.
- run_command: un shell MUY restringido (lista blanca de comandos exactos,
  nunca shell=True, argumentos como lista — no hay forma de inyectar
  "; rm -rf /" via un argumento). Existe para dos cosas que antes eran
  imposibles: instalar dependencias (npm install) y armar el scaffolding
  inicial de un repo nuevo (npx create-next-app y similares). A diferencia
  de write_file, SI puede tocar package.json/package-lock.json — es
  necesario para que "npm install" funcione, y es la unica forma en que
  este modulo modifica esos archivos.
- Nunca hace commit/push — los cambios quedan en el working tree para
  revision humana antes de subirse.

MASD-0023-0006-026 (sandbox real): run_command YA NO corre el comando
directo en el host. Lo manda, vía "sudo /opt/masd/run-sandboxed.sh"
(root-owned, el unico binario que "masd" puede invocar con sudo — nunca
acceso directo al socket de Docker, que equivaldria a darle root), a un
contenedor Docker efimero: red aislada (bloquea rangos privados del VPS en
DOCKER-USER e INPUT, deja pasar internet publico), rootfs de solo lectura
(salvo /tmp y el propio worktree montado como /workspace), sin
capabilities, memoria/CPU/PIDs acotados. "npm install <paquete>" sigue
ejecutando codigo de terceros con sus scripts de instalacion, pero ahora
ese codigo corre adentro del contenedor, no en el VPS.

RIESGO RESIDUAL: esto cubre run_command (el punto que ejecuta codigo de
terceros). read_file/write_file/grep_files siguen operando directo sobre
el worktree del host (ya estaban acotados a un usuario Linux sin
privilegios, "masd", con acceso de red restringido — ver apply-firewall.sh
— pero no corren dentro de un contenedor).
"""
import os
import re
import subprocess
import json

REPO_ROOT = os.path.realpath(os.environ.get('MASD_REPO_ROOT', '/root/portal-architechia'))


def set_repo_root(path: str):
    """
    Cambia el repo raiz sobre el que operan estas herramientas. Lo llama
    masd_worker.py al principio de cada tarea CODE si el dispatcher le paso
    un repoPath (worktree aislado de la tarea) — asi el agente lee/escribe
    en su propia copia del repo, no en el working tree principal donde
    corre el servidor Next.js en vivo.
    """
    global REPO_ROOT
    REPO_ROOT = os.path.realpath(path)

# Prefijos y archivos donde SI se permite escribir (relativos a REPO_ROOT). Cubre un proyecto Node tipico
# con o sin carpeta src/ (create-next-app --app deja app/ en la raiz), sus pruebas, documentacion,
# migraciones NUEVAS de Prisma y los archivos para empaquetarlo. Un repo puede AMPLIAR esta lista con un
# .masd-policy.json en su raiz — {"escritura": ["prisma/schema.prisma", "backend/"]} — que escribe una
# persona: el agente no puede modificarlo (esta protegido abajo).
WRITE_ALLOWED_PREFIXES = [
    'src/', 'app/', 'pages/', 'components/', 'lib/',
    'tests/', 'test/', '__tests__/', 'docs/', 'public/', 'scripts/',
    'prisma/migrations/',
]
WRITE_ALLOWED_FILES = {'README.md', 'Dockerfile', '.dockerignore'}
POLICY_FILE = '.masd-policy.json'

# Nunca leer ni escribir, sin importar la politica del repo: secretos, historial, dependencias instaladas, CI
# (los workflows de .github corren con los secretos del repositorio en cuanto alguien hace push) y la propia politica.
HARD_BLOCKED_PATTERNS = [
    re.compile(r'(^|/)\.env'),
    re.compile(r'(^|/)\.git(/|$)'),
    re.compile(r'(^|/)node_modules(/|$)'),
    re.compile(r'(^|/)\.github(/|$)'),
    re.compile(r'(^|/)\.masd-policy\.json$'),
]
# Ni leer ni escribir: pueden traer secretos en claro (variables de pm2) o pesar demasiado.
READ_BLOCKED_PATTERNS = [
    re.compile(r'(^|/)ecosystem\.config\.'),
    re.compile(r'(^|/)package-lock\.json$'),
    re.compile(r'(^|/)pnpm-lock\.yaml$'),
]
# SI se pueden LEER (el agente necesita ver scripts, dependencias y el esquema para trabajar) pero no escribir,
# salvo que la politica del repo lo autorice: package.json se cambia con "npm install".
SOFT_WRITE_BLOCKED_PATTERNS = [
    re.compile(r'(^|/)package\.json$'),
    re.compile(r'(^|/)prisma/schema\.prisma$'),
    re.compile(r'(^|/)next\.config\.'),
]
# Compatibilidad: todo lo que antes estaba bloqueado para escritura.
BLOCKED_PATTERNS = HARD_BLOCKED_PATTERNS + READ_BLOCKED_PATTERNS + SOFT_WRITE_BLOCKED_PATTERNS

DEFAULT_EXCLUDE_DIRS = {'.git', 'node_modules', '.next', 'dist', 'build'}


class FileToolError(Exception):
    pass


def _resolve(rel_path: str) -> str:
    """Resuelve rel_path contra REPO_ROOT, bloqueando path traversal."""
    if rel_path.startswith('/'):
        rel_path = rel_path.lstrip('/')
    full = os.path.realpath(os.path.join(REPO_ROOT, rel_path))
    if not (full == REPO_ROOT or full.startswith(REPO_ROOT + os.sep)):
        raise FileToolError(f'Ruta fuera del repositorio permitido: {rel_path}')
    return full


def _norm(rel_path: str) -> str:
    return rel_path.replace(os.sep, '/').lstrip('/')


def _is_blocked(rel_path: str) -> bool:
    """¿Esta prohibido LEER esta ruta?"""
    n = _norm(rel_path)
    return any(p.search(n) for p in HARD_BLOCKED_PATTERNS + READ_BLOCKED_PATTERNS)


def _politica_escritura() -> list:
    """Rutas extra que la politica del repo (.masd-policy.json, escrita por una persona) permite escribir."""
    try:
        with open(os.path.join(REPO_ROOT, POLICY_FILE), 'r', encoding='utf-8') as fh:
            extra = json.load(fh).get('escritura', [])
        return [e.lstrip('/') for e in extra if isinstance(e, str) and e and '..' not in e.split('/')]
    except Exception:
        return []


def _write_denied(rel_path: str):
    """None si se puede escribir esa ruta; si no, el motivo en texto."""
    n = _norm(rel_path)
    if any(p.search(n) for p in HARD_BLOCKED_PATTERNS + READ_BLOCKED_PATTERNS):
        return 'ruta protegida (secretos, historial, dependencias, CI, archivos de bloqueo o la politica del repo)'
    extra = _politica_escritura()
    por_politica = any(n == e or (e.endswith('/') and n.startswith(e)) for e in extra)
    if any(p.search(n) for p in SOFT_WRITE_BLOCKED_PATTERNS) and not por_politica:
        return f'archivo de configuracion/dependencias: package.json se cambia con "npm install"; el resto lo autoriza la politica del repo ({POLICY_FILE})'
    if n in WRITE_ALLOWED_FILES or any(n.startswith(p) for p in WRITE_ALLOWED_PREFIXES) or por_politica:
        return None
    return (f'fuera de las rutas permitidas ({", ".join(WRITE_ALLOWED_PREFIXES)} y {", ".join(sorted(WRITE_ALLOWED_FILES))}; '
            f'el repo puede ampliarlas con {POLICY_FILE})')


def list_files(rel_dir: str = '.', max_entries: int = 200) -> str:
    full_dir = _resolve(rel_dir)
    if not os.path.isdir(full_dir):
        return f'ERROR: {rel_dir} no es un directorio'
    entries = []
    for root, dirs, files in os.walk(full_dir):
        dirs[:] = [d for d in dirs if d not in DEFAULT_EXCLUDE_DIRS and not d.startswith('.')]
        for f in files:
            rel = os.path.relpath(os.path.join(root, f), REPO_ROOT)
            entries.append(rel)
            if len(entries) >= max_entries:
                return '\n'.join(entries) + f'\n... (truncado a {max_entries} archivos)'
    return '\n'.join(entries) if entries else '(vacio)'


MAX_FILE_BYTES = 2 * 1024 * 1024   # mas que esto no se lee entero: usar grep_files para ubicar lo que se busca
MAX_RANGE_CHARS = 12000            # tope de un rango pedido con start_line/end_line
DEFAULT_RANGE_LINES = 200


def read_file(rel_path: str, max_chars: int = 6000, start_line=None, end_line=None) -> str:
    """
    Sin rango: devuelve el principio del archivo (hasta max_chars) y, si se corta, avisa cuantas lineas
    tiene para seguir con start_line/end_line. Con rango (1-based, inclusivo): devuelve ESAS lineas, con
    un encabezado "[archivo — lineas a-b de N]". Asi un archivo grande se lee por partes y se edita con
    edit_file, sin tener que reescribirlo entero.
    """
    if _is_blocked(rel_path):
        return f'ERROR: lectura bloqueada por seguridad: {rel_path}'
    full = _resolve(rel_path)
    if not os.path.isfile(full):
        return f'ERROR: {rel_path} no existe'
    try:
        if os.path.getsize(full) > MAX_FILE_BYTES:
            return f'ERROR: {rel_path} pesa mas de {MAX_FILE_BYTES // (1024 * 1024)} MB y no se lee entero; usa grep_files para ubicar lo que buscas.'
        with open(full, 'r', encoding='utf-8', errors='replace') as fh:
            text = fh.read()
    except Exception as e:
        return f'ERROR leyendo {rel_path}: {e}'
    lineas = text.splitlines(keepends=True)
    total = len(lineas)
    if start_line is not None or end_line is not None:
        try:
            ini = max(1, int(start_line or 1))
            fin = int(end_line) if end_line is not None else ini + DEFAULT_RANGE_LINES - 1
        except (TypeError, ValueError):
            return 'ERROR: start_line y end_line deben ser numeros enteros'
        if ini > total:
            return f'ERROR: {rel_path} tiene {total} lineas; start_line={ini} esta fuera de rango'
        fin = min(total, max(fin, ini))
        trozo = ''.join(lineas[ini - 1:fin])
        aviso = ''
        if len(trozo) > MAX_RANGE_CHARS:
            trozo = trozo[:MAX_RANGE_CHARS]
            aviso = f'\n... (rango truncado a {MAX_RANGE_CHARS} caracteres: pedi un rango mas corto)'
        return f'[{rel_path} — lineas {ini}-{fin} de {total}]\n{trozo}{aviso}'
    if len(text) > max_chars:
        return (text[:max_chars] + f'\n... (truncado a {max_chars} caracteres; el archivo tiene {total} lineas y {len(text)} caracteres: '
                'lee el resto con start_line/end_line, y para modificarlo usa edit_file en vez de reescribirlo con write_file)')
    return text


GREP_INCLUDE_EXTS = ['*.ts', '*.tsx', '*.js', '*.jsx', '*.mjs', '*.cjs', '*.html', '*.css', '*.scss', '*.json', '*.md', '*.py',
                     '*.prisma', '*.sql', '*.yml', '*.yaml', '*.toml', '*.sh', '*.vue', '*.svelte', '*.txt']
GREP_EXCLUDE_DIRS = ['node_modules', '.git', '.next', '.github', 'dist', 'build']


def grep_files(pattern: str, rel_dir: str = '.', max_results: int = 40) -> str:
    # Bug real encontrado en produccion: antes solo incluia .ts/.tsx/.js/.jsx,
    # asi que grep_files sobre un .html (ej. una landing) devolvia siempre
    # "(sin resultados)" aunque el patron buscado existiera de verdad en el
    # archivo — le rompio una tarea de QA real (Sigma) que nunca pudo
    # confirmar patrones que estaban ahi.
    full_dir = _resolve(rel_dir)
    include_flags = []
    for ext in GREP_INCLUDE_EXTS:
        include_flags += ['--include=' + ext]
    for d in GREP_EXCLUDE_DIRS:
        include_flags += ['--exclude-dir=' + d]
    try:
        result = subprocess.run(
            ['grep', '-rn', *include_flags, '-e', pattern, full_dir],
            capture_output=True, text=True, timeout=15
        )
    except Exception as e:
        return f'ERROR ejecutando grep: {e}'
    lines = result.stdout.splitlines()[:max_results]
    out = []
    for line in lines:
        out.append(line.replace(REPO_ROOT + os.sep, ''))
    return '\n'.join(out) if out else '(sin resultados)'


# Proteccion contra la reescritura truncada: sin una herramienta de edicion, el unico camino para cambiar un
# archivo grande era reescribirlo entero (y read_file solo mostraba 6.000 caracteres, asi que el agente
# reescribia lo que no habia visto). Si el contenido nuevo pesa menos de la mitad que el archivo actual
# (y el archivo es grande), se rechaza salvo que se confirme con reemplazar_completo=true.
SHRINK_GUARD_MIN_BYTES = 6000
SHRINK_GUARD_RATIO = 0.5


def write_file(rel_path: str, content: str, reemplazar_completo: bool = False) -> str:
    motivo = _write_denied(rel_path)
    if motivo:
        return f'ERROR: escritura bloqueada — {rel_path}: {motivo}'
    full = _resolve(rel_path)
    if os.path.isfile(full) and not reemplazar_completo:
        try:
            previo = os.path.getsize(full)
        except OSError:
            previo = 0
        nuevo = len(content.encode('utf-8'))
        if previo > SHRINK_GUARD_MIN_BYTES and nuevo < previo * SHRINK_GUARD_RATIO:
            return (f'ERROR: {rel_path} pesa {previo} bytes y el contenido nuevo solo {nuevo}: parece una reescritura truncada. '
                    'Para cambiar una parte usa edit_file (reemplazo exacto de un trozo); si de verdad queres reemplazarlo entero, '
                    'repeti la llamada con reemplazar_completo=true.')
    os.makedirs(os.path.dirname(full), exist_ok=True)
    with open(full, 'w', encoding='utf-8') as fh:
        fh.write(content)
    return f'OK: escrito {rel_path} ({len(content)} chars)'


def edit_file(rel_path: str, old_text: str, new_text: str, replace_all: bool = False) -> str:
    """
    Reemplaza un trozo EXACTO de un archivo existente por otro (como un "buscar y reemplazar" de un solo trozo).
    Es la forma de modificar archivos grandes sin reescribirlos enteros. old_text debe aparecer una sola vez
    (agregar lineas de contexto si hace falta) salvo que se pida replace_all.
    """
    motivo = _write_denied(rel_path)
    if motivo:
        return f'ERROR: edicion bloqueada — {rel_path}: {motivo}'
    full = _resolve(rel_path)
    if not os.path.isfile(full):
        return f'ERROR: {rel_path} no existe (para crearlo usa write_file)'
    if not isinstance(old_text, str) or old_text == '':
        return 'ERROR: old_text no puede estar vacio'
    if not isinstance(new_text, str):
        return 'ERROR: new_text debe ser texto'
    if old_text == new_text:
        return 'ERROR: old_text y new_text son iguales: no hay nada que cambiar'
    try:
        if os.path.getsize(full) > MAX_FILE_BYTES:
            return f'ERROR: {rel_path} pesa mas de {MAX_FILE_BYTES // (1024 * 1024)} MB'
        with open(full, 'r', encoding='utf-8', newline='') as fh:   # newline='' conserva \r\n si el archivo lo usa
            contenido = fh.read()
    except Exception as e:
        return f'ERROR leyendo {rel_path}: {e}'
    viejo, nuevo = old_text, new_text
    n = contenido.count(viejo)
    if n == 0 and '\r\n' in contenido:
        viejo = old_text.replace('\r\n', '\n').replace('\n', '\r\n')
        nuevo = new_text.replace('\r\n', '\n').replace('\n', '\r\n')
        n = contenido.count(viejo)
    if n == 0:
        return (f'ERROR: no encontre old_text en {rel_path}. Tiene que coincidir EXACTO (espacios, sangria, saltos de linea). '
                'Lee el trozo con read_file(start_line, end_line) y copialo tal cual.')
    if n > 1 and not replace_all:
        return (f'ERROR: old_text aparece {n} veces en {rel_path}. Agrega mas lineas de contexto para que sea unico, '
                'o usa replace_all=true si queres cambiarlas todas.')
    resultado = contenido.replace(viejo, nuevo) if replace_all else contenido.replace(viejo, nuevo, 1)
    try:
        with open(full, 'w', encoding='utf-8', newline='') as fh:
            fh.write(resultado)
    except Exception as e:
        return f'ERROR escribiendo {rel_path}: {e}'
    hechos = n if replace_all else 1
    return f'OK: editado {rel_path} ({hechos} reemplazo(s), {len(resultado) - len(contenido):+d} caracteres)'


def delete_file(rel_path: str) -> str:
    """Borra un archivo (solo dentro de las rutas donde se puede escribir)."""
    motivo = _write_denied(rel_path)
    if motivo:
        return f'ERROR: borrado bloqueado — {rel_path}: {motivo}'
    full = _resolve(rel_path)
    if not os.path.isfile(full):
        return f'ERROR: {rel_path} no existe o no es un archivo'
    os.remove(full)
    return f'OK: borrado {rel_path}'


def move_file(rel_from: str, rel_to: str) -> str:
    """Mueve o renombra un archivo; origen y destino deben estar en rutas donde se puede escribir."""
    for r in (rel_from, rel_to):
        motivo = _write_denied(r)
        if motivo:
            return f'ERROR: movimiento bloqueado — {r}: {motivo}'
    origen, destino = _resolve(rel_from), _resolve(rel_to)
    if not os.path.isfile(origen):
        return f'ERROR: {rel_from} no existe o no es un archivo'
    if os.path.exists(destino):
        return f'ERROR: {rel_to} ya existe; borralo primero o elegi otro nombre'
    os.makedirs(os.path.dirname(destino), exist_ok=True)
    os.rename(origen, destino)
    return f'OK: movido {rel_from} -> {rel_to}'


# Comandos permitidos para run_command — SOLO estos, sin interprete de shell. El cwd siempre es
# REPO_ROOT (el worktree aislado de la tarea, jamas el repo principal). Cubre las dos cosas que
# le faltaban al Motor: instalar dependencias / scaffolding inicial, y correr un script que
# termine solo (build, test, lint). NO incluye "npm run dev/start/serve" (procesos que no
# terminan solos, colgarian la tarea hasta el timeout) ni npx arbitrario (solo generadores de
# scaffolding conocidos, listados a mano — no cualquier paquete de npm).
ALLOWED_COMMANDS = {
    'npm': {'install', 'i', 'ci', 'init', 'run', 'run-script'},
    'npx': {'create-next-app', 'create-vite', 'create-react-app', 'tsc', 'prisma'},
}
# De prisma solo lo que no toca ninguna base de datos: validar/formatear el esquema y generar el cliente.
# (migrate/db push/studio quedan fuera: las migraciones reales las aplica una persona, ver despliegue.rs).
ALLOWED_PRISMA_SUBCOMMANDS = {'validate', 'format', 'generate'}
BLOCKED_NPM_SCRIPTS = {'dev', 'start', 'serve', 'watch'}
COMMAND_TIMEOUT_S = 300
MAX_COMMAND_OUTPUT = 6000


# MASD-0023-0006-026: corre un comando de run_command DENTRO de un contenedor Docker efimero en
# vez de en el host — via el mediador root-owned /opt/masd/run-sandboxed.sh, el UNICO binario
# que "masd" puede invocar con sudo (nunca el socket de Docker directo). Ese script es quien
# decide de verdad los limites (red aislada, rootfs de solo lectura, memoria/CPU/PIDs, sin
# capabilities) — esta funcion solo arma el argv y traduce el resultado.
SANDBOX_RUNNER = '/opt/masd/run-sandboxed.sh'


def _correr_en_sandbox(comando: list, timeout: int):
    return subprocess.run(
        ['sudo', SANDBOX_RUNNER, REPO_ROOT] + comando,
        capture_output=True, text=True, timeout=timeout, stdin=subprocess.DEVNULL,
    )


# MASD-0023-0006-025: revision minima despues de instalar algo (npm install/i/ci no vetan el
# paquete ANTES — "npm install X" ya lo descarga y corre su script de instalacion por su cuenta,
# no hay forma de vetarlo antes sin reimplementar el resolver de npm). Lo que SI se puede hacer
# es avisar DESPUES: correr "npm audit" y devolver el resumen de vulnerabilidades conocidas junto
# con la salida del install, para que quede visible en la traza de la tarea (no bloquea nada,
# es informativo — bloquear instalaciones reales por esto tendria muchos falsos positivos).
def _resumen_npm_audit() -> str:
    try:
        r = _correr_en_sandbox(['npm', 'audit', '--json'], timeout=60)
        data = json.loads(r.stdout or '{}')
        meta = data.get('metadata', {}) or {}
        vulns = meta.get('vulnerabilities', {}) or {}
        total = sum(v for k, v in vulns.items() if k != 'total' and isinstance(v, int))
        if total == 0:
            return 'npm audit: sin vulnerabilidades conocidas.'
        partes = [f'{v} {k}' for k, v in vulns.items() if k != 'total' and isinstance(v, int) and v > 0]
        return f'npm audit: {total} vulnerabilidad(es) conocida(s) — {", ".join(partes)}. Revisar antes de confiar en esta dependencia.'
    except Exception:
        return 'npm audit: no se pudo correr (no bloqueante).'


def run_command(comando) -> str:
    if not isinstance(comando, list) or not comando or not all(isinstance(c, str) for c in comando):
        return 'ERROR: "comando" debe ser una lista de strings, ej. ["npm", "install", "zod"]'
    bin_, args = comando[0], comando[1:]
    if bin_ not in ALLOWED_COMMANDS:
        return f'ERROR: comando no permitido: "{bin_}". Solo se permite: {", ".join(sorted(ALLOWED_COMMANDS))}.'
    if not args or args[0] not in ALLOWED_COMMANDS[bin_]:
        permitidos = ", ".join(sorted(ALLOWED_COMMANDS[bin_]))
        return f'ERROR: "{bin_} {args[0] if args else ""}" no esta permitido. Subcomandos permitidos de {bin_}: {permitidos}.'
    if bin_ == 'npx' and args[0] == 'prisma':
        if len(args) < 2 or args[1] not in ALLOWED_PRISMA_SUBCOMMANDS:
            return f'ERROR: de prisma solo se permite: {", ".join("npx prisma " + s for s in sorted(ALLOWED_PRISMA_SUBCOMMANDS))}.'
    if bin_ == 'npm' and args[0] in ('run', 'run-script'):
        script = args[1] if len(args) > 1 else ''
        if script.lower() in BLOCKED_NPM_SCRIPTS:
            return f'ERROR: "npm run {script}" no esta permitido (arranca un proceso que no termina solo, colgaria la tarea). Usa build/test/lint u otro script que efectivamente corte.'
    # Capa extra contra flags que redirigirian la ejecucion fuera del worktree (ej. --prefix,
    # -C, una ruta absoluta pasada como argumento) — el cwd ya esta fijado a REPO_ROOT, esto
    # bloquea el intento aunque el subcomando en si este permitido.
    for a in args:
        if a.startswith('/') or '..' in a.replace('\\', '/').split('/'):
            return f'ERROR: argumento no permitido (ruta absoluta o ".."): {a}'
    try:
        result = _correr_en_sandbox(comando, timeout=COMMAND_TIMEOUT_S)
    except subprocess.TimeoutExpired:
        return f'ERROR: el comando tardo mas de {COMMAND_TIMEOUT_S}s y se corto.'
    except Exception as e:
        return f'ERROR ejecutando el comando: {e}'
    if result.returncode == 3 and 'fuera de' in (result.stderr or ''):
        # El mediador rechazo el worktree (ver run-sandboxed.sh) — pasa cuando run_command se
        # invoca fuera de una tarea real (ej. una sesion sin worktree asignado, usando el repo
        # principal como REPO_ROOT por defecto). Nunca deberia correr codigo de terceros ahi.
        return f'ERROR: run_command solo esta disponible dentro de una tarea con worktree propio, no en el repo principal. ({result.stderr.strip()})'
    salida = (result.stdout or '') + (('\n' + result.stderr) if result.stderr else '')
    salida = salida.strip() or '(sin salida)'
    if len(salida) > MAX_COMMAND_OUTPUT:
        salida = salida[:MAX_COMMAND_OUTPUT] + f'\n... (truncado a {MAX_COMMAND_OUTPUT} caracteres)'
    if bin_ == 'npm' and args[0] in ('install', 'i', 'ci') and result.returncode == 0:
        salida = f'{salida}\n\n{_resumen_npm_audit()}'
    return f'[exit {result.returncode}]\n{salida}'


TOOL_SCHEMAS = [
    {
        'type': 'function',
        'function': {
            'name': 'list_files',
            'description': 'Lista archivos dentro de un directorio del repositorio (recursivo, excluye node_modules/.next/.git).',
            'parameters': {
                'type': 'object',
                'properties': {'rel_dir': {'type': 'string', 'description': 'Directorio relativo a la raiz del repo, ej. "src/components"'}},
            },
        },
    },
    {
        'type': 'function',
        'function': {
            'name': 'read_file',
            'description': (
                'Lee un archivo del repositorio. Sin rango devuelve el principio (hasta ~6000 caracteres) y, si se corta, '
                'te dice cuantas lineas tiene. Para archivos largos pedi el trozo que necesitas con start_line/end_line '
                '(1-based, inclusivo; maximo ~12000 caracteres por pedido). Podes leer package.json y el esquema de Prisma.'
            ),
            'parameters': {
                'type': 'object',
                'properties': {
                    'rel_path': {'type': 'string', 'description': 'Ruta relativa a la raiz del repo, ej. "src/components/Header.tsx"'},
                    'start_line': {'type': 'integer', 'description': 'Primera linea a leer (opcional, empieza en 1)'},
                    'end_line': {'type': 'integer', 'description': 'Ultima linea a leer (opcional; por defecto 200 lineas desde start_line)'},
                },
                'required': ['rel_path'],
            },
        },
    },
    {
        'type': 'function',
        'function': {
            'name': 'grep_files',
            'description': 'Busca un patron de texto en los archivos de codigo y configuracion del repositorio (ts, tsx, js, json, md, py, prisma, sql, yml, css, html...). Excluye node_modules y .git.',
            'parameters': {
                'type': 'object',
                'properties': {
                    'pattern': {'type': 'string', 'description': 'Patron a buscar (texto o regex basico de grep)'},
                    'rel_dir': {'type': 'string', 'description': 'Directorio donde buscar, por defecto la raiz del repo'},
                },
                'required': ['pattern'],
            },
        },
    },
    {
        'type': 'function',
        'function': {
            'name': 'write_file',
            'description': (
                'Crea un archivo nuevo, o reemplaza uno entero. Para CAMBIAR UNA PARTE de un archivo que ya existe usa edit_file: '
                'si el contenido nuevo es mucho mas corto que el actual se rechaza (parece una reescritura truncada). '
                'Rutas permitidas: src/, app/, pages/, components/, lib/, tests/, test/, __tests__/, docs/, public/, scripts/, '
                'prisma/migrations/ y los archivos README.md, Dockerfile y .dockerignore. No toca .env, .git, .github ni node_modules; '
                'package.json se modifica con "npm install". No hace commit ni push — el cambio queda pendiente de revision humana.'
            ),
            'parameters': {
                'type': 'object',
                'properties': {
                    'rel_path': {'type': 'string', 'description': 'Ruta relativa, ej. "src/components/NewWidget.tsx" o "tests/mediana.test.ts"'},
                    'content': {'type': 'string', 'description': 'Contenido completo del archivo'},
                    'reemplazar_completo': {'type': 'boolean', 'description': 'true solo si de verdad queres reemplazar entero un archivo grande por una version mucho mas corta'},
                },
                'required': ['rel_path', 'content'],
            },
        },
    },
    {
        'type': 'function',
        'function': {
            'name': 'edit_file',
            'description': (
                'Cambia UN trozo de un archivo que ya existe: reemplaza old_text (que debe coincidir EXACTO, con sus espacios y saltos de linea) '
                'por new_text. Es la forma correcta de modificar archivos medianos o grandes, sin reescribirlos enteros. Si old_text aparece mas de una vez '
                'agrega lineas de contexto para que sea unico (o usa replace_all). Para varios cambios, llamala varias veces. Mismas rutas permitidas que write_file.'
            ),
            'parameters': {
                'type': 'object',
                'properties': {
                    'rel_path': {'type': 'string', 'description': 'Archivo a modificar, ej. "src/lib/inventario.ts"'},
                    'old_text': {'type': 'string', 'description': 'Texto actual a reemplazar, copiado tal cual del archivo (con read_file)'},
                    'new_text': {'type': 'string', 'description': 'Texto nuevo'},
                    'replace_all': {'type': 'boolean', 'description': 'Reemplazar todas las apariciones de old_text (por defecto solo una, y debe ser unica)'},
                },
                'required': ['rel_path', 'old_text', 'new_text'],
            },
        },
    },
    {
        'type': 'function',
        'function': {
            'name': 'delete_file',
            'description': 'Borra un archivo (solo dentro de las rutas donde se puede escribir). Util en refactors y migraciones.',
            'parameters': {
                'type': 'object',
                'properties': {'rel_path': {'type': 'string', 'description': 'Archivo a borrar'}},
                'required': ['rel_path'],
            },
        },
    },
    {
        'type': 'function',
        'function': {
            'name': 'move_file',
            'description': 'Mueve o renombra un archivo (origen y destino dentro de las rutas donde se puede escribir). El destino no debe existir.',
            'parameters': {
                'type': 'object',
                'properties': {
                    'rel_from': {'type': 'string', 'description': 'Ruta actual'},
                    'rel_to': {'type': 'string', 'description': 'Ruta nueva'},
                },
                'required': ['rel_from', 'rel_to'],
            },
        },
    },
    {
        'type': 'function',
        'function': {
            'name': 'run_command',
            'description': (
                'Ejecuta UN comando de una lista blanca fija dentro del repositorio: instalar '
                'dependencias (npm install/ci), scaffolding inicial de un proyecto NUEVO (npx '
                'create-next-app, create-vite, create-react-app — usalo si el repo esta vacio o '
                'casi vacio), o correr un script de package.json que termine solo (build, test, '
                'lint — NUNCA dev/start/serve, esos no terminan y colgarian la tarea). No es una '
                'shell general: cualquier otro comando se rechaza.'
            ),
            'parameters': {
                'type': 'object',
                'properties': {
                    'comando': {
                        'type': 'array', 'items': {'type': 'string'},
                        'description': 'El comando como lista de argumentos (nunca un string con espacios), ej. ["npm","install","zod"] o ["npx","create-next-app",".","--typescript","--eslint","--tailwind","--app","--yes"]',
                    },
                },
                'required': ['comando'],
            },
        },
    },
]


def execute_tool(name: str, args: dict) -> str:
    try:
        if name == 'list_files':
            return list_files(args.get('rel_dir', '.'))
        if name == 'read_file':
            return read_file(args['rel_path'], start_line=args.get('start_line'), end_line=args.get('end_line'))
        if name == 'grep_files':
            return grep_files(args['pattern'], args.get('rel_dir', '.'))
        if name == 'write_file':
            return write_file(args['rel_path'], args['content'], reemplazar_completo=bool(args.get('reemplazar_completo', False)))
        if name == 'edit_file':
            return edit_file(args['rel_path'], args['old_text'], args['new_text'], replace_all=bool(args.get('replace_all', False)))
        if name == 'delete_file':
            return delete_file(args['rel_path'])
        if name == 'move_file':
            return move_file(args['rel_from'], args['rel_to'])
        if name == 'run_command':
            return run_command(args.get('comando', []))
        return f'ERROR: herramienta desconocida: {name}'
    except FileToolError as e:
        return f'ERROR: {e}'
    except KeyError as e:
        return f'ERROR: falta el argumento {e}'
    except Exception as e:
        # Mismo fix real que en code_index.py: un error inesperado (ej. el
        # worktree desaparecio a mitad de ejecucion) no debe reventar toda
        # la tarea — se devuelve como resultado de la herramienta.
        return f'ERROR inesperado en herramienta de archivo ({name}): {e}'
