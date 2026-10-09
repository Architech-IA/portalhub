"""
code_index.py — Indice de codigo real via tree-sitter para agentes CODE
del Motor Agentico SDD.

En vez de que el agente adivine la estructura del repo leyendo archivo
por archivo, este modulo parsea cada .ts/.tsx/.js/.jsx con tree-sitter
y extrae: simbolos exportados (funciones/clases/consts/interfaces/types)
e imports — con eso arma un grafo de dependencias (quien importa a
quien) que se puede consultar directo, sin leer nada.

El indice se persiste en un JSON dentro del repo y se reconstruye solo
si esta vencido (TTL) o no existe — no hay que mantenerlo a mano.
"""
import os
import re
import json
import time

from tree_sitter_language_pack import get_parser

REPO_ROOT = os.path.realpath(os.environ.get('MASD_REPO_ROOT', '/root/portal-architechia'))
SRC_DIR = os.path.join(REPO_ROOT, 'src')
INDEX_PATH = os.path.join(REPO_ROOT, '.masd_code_index.json')
INDEX_MAX_AGE_S = 300  # 5 minutos — despues de eso se reconstruye en la siguiente consulta


def set_repo_root(path: str):
    """Ver file_tools.set_repo_root — mismo motivo: apuntar al worktree
    aislado de la tarea CODE en curso en vez del repo principal."""
    global REPO_ROOT, SRC_DIR, INDEX_PATH
    REPO_ROOT = os.path.realpath(path)
    SRC_DIR = os.path.join(REPO_ROOT, 'src')
    INDEX_PATH = os.path.join(REPO_ROOT, '.masd_code_index.json')

EXT_TO_LANG = {'.ts': 'typescript', '.tsx': 'tsx', '.js': 'javascript', '.jsx': 'tsx'}
EXCLUDE_DIRS = {'node_modules', '.next', 'dist', 'build', '.git'}

_PARSERS = {}


def _parser_for(ext: str):
    lang = EXT_TO_LANG.get(ext)
    if not lang:
        return None
    if lang not in _PARSERS:
        _PARSERS[lang] = get_parser(lang)
    return _PARSERS[lang]


_EXPORT_NAME_RE = re.compile(r'(function|class|const|let|interface|type|enum)\s+([A-Za-z0-9_]+)')
_IMPORT_FROM_RE = re.compile(r'''from\s+['"]([^'"]+)['"]''')


def _extract_file(full_path: str, ext: str) -> dict:
    parser = _parser_for(ext)
    if parser is None:
        return {'exports': [], 'imports': []}
    with open(full_path, 'rb') as fh:
        src = fh.read()
    tree = parser.parse(src)

    exports, imports = [], []

    def visit(node):
        if node.type == 'export_statement':
            text = src[node.start_byte:node.end_byte].decode('utf-8', 'replace')
            m = _EXPORT_NAME_RE.search(text)
            if m:
                exports.append({'kind': m.group(1), 'name': m.group(2), 'line': node.start_point[0] + 1})
        elif node.type == 'import_statement':
            text = src[node.start_byte:node.end_byte].decode('utf-8', 'replace')
            m = _IMPORT_FROM_RE.search(text)
            if m:
                imports.append(m.group(1))
        for child in node.children:
            visit(child)

    visit(tree.root_node)
    return {'exports': exports, 'imports': imports}


def build_index() -> dict:
    files = {}
    for root, dirs, filenames in os.walk(SRC_DIR):
        dirs[:] = [d for d in dirs if d not in EXCLUDE_DIRS and not d.startswith('.')]
        for fn in filenames:
            ext = os.path.splitext(fn)[1]
            if ext not in EXT_TO_LANG:
                continue
            full = os.path.join(root, fn)
            rel = os.path.relpath(full, REPO_ROOT).replace(os.sep, '/')
            try:
                files[rel] = _extract_file(full, ext)
            except Exception as e:
                files[rel] = {'exports': [], 'imports': [], 'error': str(e)}

    # Grafo inverso: para cada modulo importado, quien lo importa
    dependents = {}
    for rel, info in files.items():
        for imp in info.get('imports', []):
            dependents.setdefault(imp, []).append(rel)

    index = {
        'builtAt': time.time(),
        'fileCount': len(files),
        'files': files,
        'dependents': dependents,
    }
    with open(INDEX_PATH, 'w', encoding='utf-8') as fh:
        json.dump(index, fh)
    return index


def load_index() -> dict:
    if os.path.exists(INDEX_PATH):
        age = time.time() - os.path.getmtime(INDEX_PATH)
        if age < INDEX_MAX_AGE_S:
            try:
                with open(INDEX_PATH, 'r', encoding='utf-8') as fh:
                    return json.load(fh)
            except Exception:
                pass
    return build_index()


def find_symbol(name: str, limit: int = 20) -> str:
    idx = load_index()
    q = name.lower()
    results = []
    for rel, info in idx['files'].items():
        for exp in info.get('exports', []):
            if q in exp['name'].lower():
                results.append(f"{exp['kind']} {exp['name']}  →  {rel}:{exp['line']}")
    if not results:
        return f'Sin coincidencias para "{name}"'
    return '\n'.join(results[:limit])


def get_file_summary(rel_path: str) -> str:
    idx = load_index()
    info = idx['files'].get(rel_path)
    if info is None:
        return f'ERROR: {rel_path} no esta indexado (¿existe y es .ts/.tsx/.js/.jsx dentro de src/?)'
    exports = info.get('exports', [])
    imports = info.get('imports', [])
    lines = [f'Archivo: {rel_path}']
    lines.append('Exports: ' + (', '.join(f"{e['kind']} {e['name']}" for e in exports) or '(ninguno)'))
    lines.append('Imports: ' + (', '.join(imports) or '(ninguno)'))
    return '\n'.join(lines)


def get_dependents(module_or_path: str, limit: int = 30) -> str:
    idx = load_index()
    matches = set()
    for spec, deps in idx['dependents'].items():
        if module_or_path in spec or spec in module_or_path:
            matches.update(deps)
    if not matches:
        return f'Nadie importa "{module_or_path}" (o no se encontro coincidencia)'
    return '\n'.join(sorted(matches)[:limit])


TOOL_SCHEMAS = [
    {
        'type': 'function',
        'function': {
            'name': 'find_symbol',
            'description': 'Busca una funcion/clase/const/interface exportada por nombre en TODO el repo indexado, sin leer archivos. Devuelve archivo:linea de cada coincidencia.',
            'parameters': {
                'type': 'object',
                'properties': {'name': {'type': 'string', 'description': 'Nombre (o parte del nombre) del simbolo a buscar, ej. "ContactForm"'}},
                'required': ['name'],
            },
        },
    },
    {
        'type': 'function',
        'function': {
            'name': 'get_file_summary',
            'description': 'Da un resumen estructural de un archivo (que exporta, que importa) sin leer su contenido completo. Mas barato que read_file cuando solo necesitas saber que hay ahi.',
            'parameters': {
                'type': 'object',
                'properties': {'rel_path': {'type': 'string', 'description': 'Ruta relativa del archivo, ej. "src/components/Header.tsx"'}},
                'required': ['rel_path'],
            },
        },
    },
    {
        'type': 'function',
        'function': {
            'name': 'get_dependents',
            'description': 'Lista que archivos importan un modulo o ruta dado — util antes de modificar algo, para saber a quien puede afectar.',
            'parameters': {
                'type': 'object',
                'properties': {'module_or_path': {'type': 'string', 'description': 'Especificador de import o ruta a buscar, ej. "@/lib/prisma" o "vaultNotes"'}},
                'required': ['module_or_path'],
            },
        },
    },
]


def execute_tool(name: str, args: dict) -> str:
    try:
        if name == 'find_symbol':
            return find_symbol(args['name'])
        if name == 'get_file_summary':
            return get_file_summary(args['rel_path'])
        if name == 'get_dependents':
            return get_dependents(args['module_or_path'])
        return f'ERROR: herramienta de indice desconocida: {name}'
    except KeyError as e:
        return f'ERROR: falta el argumento {e}'
    except Exception as e:
        # BUG REAL encontrado en produccion: solo se atrapaba KeyError — un
        # error real inesperado construyendo o leyendo el indice (ej. el
        # worktree de la tarea desaparecio a mitad de ejecucion por una
        # condicion de carrera de otro dispatch concurrente sobre el mismo
        # taskCode) tiraba una excepcion cruda que reventaba la tarea ENTERA
        # sin ningun mensaje util — el modelo se quedaba sin poder seguir y
        # el worker reportaba un error de Python crudo en vez de un fallo
        # manejable. Ahora se devuelve como resultado de la herramienta (el
        # modelo puede leerlo y decidir seguir sin este dato, en vez de que
        # toda la tarea muera).
        return f'ERROR inesperado en herramienta de indice de codigo ({name}): {e}'
