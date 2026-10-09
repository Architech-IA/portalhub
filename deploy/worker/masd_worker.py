"""
masd_worker.py — Worker del Motor Agentico SDD sobre el Harness
Hace pull de la cola (Harness.next_task), llama a OpenCode GO,
y reporta el resultado de vuelta al portal Next.js.

Las tareas de strategy=CODE reciben herramientas reales de archivo
(list_files/read_file/grep_files/write_file, ver file_tools.py) para
que efectivamente lean y modifiquen el repo, en vez de solo describir
codigo como texto. Este worker nunca commitea ni pushea nada — eso lo
hace el portal (taskDispatcher.finalizeExecution) sobre el worktree
aislado de la tarea (ver gitWorktree.ts) despues de verificar que el
resultado paso los chequeos reales.

Correr N instancias (via pm2) para limitar la concurrencia real a N.
"""
import os
import re
import sys
import time
import json
import requests

# Algunos modelos razonadores devuelven su cadena de pensamiento inline
# dentro del propio campo "content" entre tags <think>...</think>, en vez de
# un canal separado. Se vio en vivo: un resultado real se guardo con el
# razonamiento crudo filtrado adentro. Se lo saca antes de guardar nada.
_THINK_TAG_RE = re.compile(r'<think>.*?</think>', re.DOTALL | re.IGNORECASE)


def strip_reasoning_tags(text: str) -> str:
    cleaned = _THINK_TAG_RE.sub('', text)
    # Si el <think> quedo sin cerrar (se corto por max_tokens a mitad de
    # razonamiento), no hay match del regex de arriba — cortar todo desde
    # ahi en vez de guardar razonamiento crudo sin terminar.
    dangling = cleaned.lower().find('<think>')
    if dangling != -1:
        cleaned = cleaned[:dangling]
    return cleaned.strip()

sys.path.insert(0, '/root')
sys.path.insert(0, os.path.dirname(os.path.realpath(__file__)))
from harness import Harness, TaskStatus  # noqa: E402
import file_tools  # noqa: E402
import code_index  # noqa: E402

ALL_TOOL_SCHEMAS = file_tools.TOOL_SCHEMAS + code_index.TOOL_SCHEMAS
CODE_INDEX_TOOL_NAMES = {'find_symbol', 'get_file_summary', 'get_dependents'}

WORKER_ID = os.environ.get('MASD_WORKER_ID', '1')
OPENCODE_KEY = os.environ.get('OPENCODE_API_KEY', '')
CALLBACK_URL = os.environ.get('EXECUTOR_CALLBACK_URL', 'http://127.0.0.1:3003/api/executor/complete')
EVENT_URL = os.environ.get('EXECUTOR_EVENT_URL', 'http://127.0.0.1:3003/api/executor/event')
# Callbacks separados para tareas de tipo explain_task/plan_task (mini-agentes
# de solo-lectura, ver process_investigation_task): NUNCA deben pasar por
# finalizeExecution (ese callback hace merge/descarte de worktrees y cambia
# el status del BacklogItem real) — "Explicar" y "Proponer plan" son de solo
# lectura sobre el estado del repo, nunca una ejecucion real de la tarea.
EXPLAIN_CALLBACK_URL = os.environ.get('EXECUTOR_EXPLAIN_CALLBACK_URL', 'http://127.0.0.1:3003/api/executor/explain-complete')
PLAN_CALLBACK_URL = os.environ.get('EXECUTOR_PLAN_CALLBACK_URL', 'http://127.0.0.1:3003/api/executor/plan-complete')
INTERNAL_API_KEY = os.environ.get('INTERNAL_API_KEY', '')
POLL_INTERVAL_S = 2
MAX_TOOL_ITERATIONS = 20  # techo duro: nunca mas de 20 vueltas de herramientas por tarea
# Antes 8 — muy justo para una task real de codigo con varios archivos a
# explorar/leer/escribir (ej. "OAuth2 M365 & MSAL Integration"): se probo
# real con el aviso de "te queda 1 paso" ya puesto y siguio fallando por el
# mismo motivo. La mitigacion del aviso reduce el desperdicio de pasos, pero
# si la tarea necesita mas de 8 de entrada, ningun aviso alcanza. Subido a
# 20 como primer intento antes de considerar partir las tasks del plan en
# piezas mas chicas.


def log(msg: str):
    print(f"[masd-worker-{WORKER_ID}] {msg}", flush=True)


# BUG REAL encontrado en produccion: un error 500 transitorio del propio
# proveedor del modelo ("Internal server error", visto en vivo con
# "Design Partner Onboarding & Feedback", tarea sin ninguna relacion con el
# fallo mas que la mala suerte del timing) tiraba una excepcion de una sola
# vez y la tarea entera se marcaba FAILED — sin ningun reintento, aunque el
# error no tuviera nada que ver con la tarea ni con el codigo, solo con que
# el servidor de OpenCode tuvo un hiccup momentaneo. Se agrega un reintento
# real con backoff exponencial SOLO para errores claramente transitorios
# (5xx) — un 4xx (ej. modelo invalido, auth, payload mal formado) no se
# reintenta porque reintentar no va a cambiar nada, es un error real.
TRANSIENT_RETRY_COUNT = 3
TRANSIENT_BACKOFF_BASE_S = 3


def call_opencode_once(api_url: str, model_id: str, messages: list, max_tokens: int, timeout_s: int, tools=None, session_id: str = '') -> dict:
    body = {'model': model_id, 'messages': messages, 'max_tokens': max_tokens}
    if tools:
        body['tools'] = tools

    last_err = None
    for attempt in range(TRANSIENT_RETRY_COUNT):
        try:
            resp = requests.post(
                api_url,
                headers={
                    'Content-Type': 'application/json', 'Authorization': f'Bearer {OPENCODE_KEY}',
                    # BUG REAL encontrado en evals (MASD-0023-0006-014): faltaba este header.
                    # La API GO de OpenCode lo exige (400 MissingSessionID sin el) — toda tarea
                    # CODE fallaba al instante, en la primera llamada, sin gastar ningun tool call.
                    # Un id estable por TAREA (no por llamada) alcanza, igual que el lado Node
                    # (opencodeChat.ts) usa un id estable por sesion/recurso.
                    **({'x-opencode-session': session_id} if session_id else {}),
                },
                json=body,
                timeout=timeout_s,
            )
        except requests.exceptions.RequestException as e:
            last_err = e
            if attempt < TRANSIENT_RETRY_COUNT - 1:
                wait_s = TRANSIENT_BACKOFF_BASE_S * (2 ** attempt)
                log(f'ADVERTENCIA: error de red llamando a OpenCode ({e}) — reintento {attempt + 1}/{TRANSIENT_RETRY_COUNT} en {wait_s}s')
                time.sleep(wait_s)
                continue
            raise RuntimeError(f'Error de red llamando a OpenCode tras {TRANSIENT_RETRY_COUNT} intentos: {e}') from last_err

        if resp.ok:
            return resp.json()

        if resp.status_code >= 500 and attempt < TRANSIENT_RETRY_COUNT - 1:
            wait_s = TRANSIENT_BACKOFF_BASE_S * (2 ** attempt)
            log(f'ADVERTENCIA: OpenCode devolvio {resp.status_code} (transitorio) — reintento {attempt + 1}/{TRANSIENT_RETRY_COUNT} en {wait_s}s')
            time.sleep(wait_s)
            continue

        raise RuntimeError(f'OpenCode API error {resp.status_code}: {resp.text[:300]}')

    raise RuntimeError(f'OpenCode API error tras {TRANSIENT_RETRY_COUNT} intentos')


def emit_event(task_id: str, exec_id: str, kind: str, message: str) -> None:
    """
    Narra en vivo lo que va haciendo el loop de herramientas para la Sala de
    Control del portal (grafo + traza por tarea). Fire-and-forget real: un
    timeout corto y cualquier excepcion se traga sin loggear siquiera en
    nivel ADVERTENCIA — esto es solo observabilidad, nunca puede frenar ni
    una fraccion de segundo la ejecucion real de la tarea por un problema de
    red hacia el endpoint de eventos.
    """
    try:
        headers = {'Content-Type': 'application/json'}
        if INTERNAL_API_KEY:
            headers['X-Api-Key'] = INTERNAL_API_KEY
        requests.post(
            EVENT_URL, headers=headers,
            json={'taskId': task_id, 'execId': exec_id, 'kind': kind, 'message': message},
            timeout=3, allow_redirects=False,
        )
    except Exception:
        pass


def run_agent(api_url: str, model_id: str, system_prompt: str, user_prompt: str, strategy: str, task_id: str = '', exec_id: str = '') -> tuple[str, list, dict]:
    """
    Ejecuta el agente. Si strategy == CODE, habilita herramientas de
    archivo y hace el loop de tool-calling hasta que el modelo responda
    con texto final (o se llegue al techo de iteraciones).
    Devuelve (texto_final, log_de_llamadas_a_herramientas, uso_de_tokens_acumulado).
    uso_de_tokens_acumulado suma prompt_tokens/completion_tokens/total_tokens
    de TODAS las llamadas reales a OpenCode que hizo esta tarea (una sola si
    es LLM, hasta MAX_TOOL_ITERATIONS si es CODE) — sin esto no hay forma de
    saber cuanto gasta realmente el motor por tarea.
    """
    # 300s para ambas: con max_tokens=8000 un LLM task tambien puede tardar
    # mas de 90s en generar (visto en pruebas reales: timeout real a los 90s
    # en una tarea LLM comun, sin reintento automatico del Harness).
    timeout_s = 300
    # 4096 para ambas: se detecto en pruebas reales que un LLM task con 2048
    # llegaba al tope de tokens (completion_tokens=2048 exacto) y el contenido
    # final visible quedaba truncado a un puñado de caracteres — parte del
    # presupuesto de tokens se va en razonamiento interno del modelo antes de
    # la respuesta visible, y 2048 no alcanzaba a cubrir ambas cosas.
    max_tokens = 8000
    use_tools = strategy == 'CODE'

    # Id estable para TODA esta tarea (todas sus llamadas/reintentos comparten uno), para que
    # el proveedor pueda enrutar/cachear el prefijo del prompt igual que en las demas rutas.
    session_id = f'masd-task-{task_id}' if task_id else f'masd-adhoc-{int(time.time())}'
    messages = [
        {'role': 'system', 'content': system_prompt + (
            '\n\nTenes herramientas para explorar y modificar el repositorio real. '
            'Preferí find_symbol/get_file_summary/get_dependents primero (son consultas '
            'exactas a un indice de codigo ya parseado, no cuestan leer archivos completos): '
            'find_symbol para ubicar una funcion/componente por nombre, get_file_summary para '
            'ver que exporta/importa un archivo sin leerlo entero, get_dependents antes de '
            'modificar algo para saber a quien puede afectar. Usá list_files/grep_files/read_file '
            'cuando necesites explorar por carpeta o ver el codigo completo de un archivo. '
            'Para CREAR un archivo nuevo usá write_file; para CAMBIAR uno que ya existe usá edit_file (reemplaza un trozo EXACTO) — no lo reescribas entero con write_file, '
            'y si el archivo es largo leelo por partes con read_file(start_line, end_line) y mirá siempre el trozo antes de editarlo. '
            'Podés escribir en src/, app/, pages/, components/, lib/, tests/, docs/, public/, scripts/, prisma/migrations/ y en README.md/Dockerfile; con delete_file y move_file borrás o renombrás. '
            'Cuando la tarea lo pida, escribí pruebas en tests/ y corrélas con run_command (npm run test). Podés leer package.json para ver qué scripts y dependencias hay. '
            'Si el repositorio está vacío o casi vacío (recién creado), usá run_command con npx create-next-app/create-vite/create-react-app para armar el scaffolding inicial ANTES de escribir código a mano — no intentes recrear a mano lo que un generador real hace mejor. Si el scaffolding es un proyecto Next.js, agregá "output: \'standalone\'" a next.config (crealo si no existe) — el despliegue arma una imagen mucho más liviana cuando está esa opción, y si no la agregás igual funciona, solo que más pesado. Usá run_command también para instalar una dependencia nueva (npm install) o correr build/test/lint. run_command NUNCA corre dev/start/serve (se cuelgan) ni un paquete de npm fuera de esa lista corta — si lo necesitás y no está permitido, decilo en tu resumen en vez de improvisar otra forma. Cuando termines, respondé con un resumen '
            'breve en texto plano de lo que hiciste. Si tomaste alguna decision de diseño o '
            'convencion que otras tareas de este MISMO sprint deberian conocer (nombre de una '
            'funcion/componente que crearon, patron de carpeta elegido, enfoque tecnico sobre '
            'algo ambiguo), agregala al final de tu resumen bajo un encabezado exacto '
            '"DECISIONES:" seguido de una lista corta (una linea por decision). Si no tomaste '
            'ninguna decision asi de relevante, escribí "DECISIONES: ninguna".'
            if use_tools else ''
        )},
        {'role': 'user', 'content': user_prompt},
    ]
    tool_log = []
    usage_total = {'prompt_tokens': 0, 'completion_tokens': 0, 'total_tokens': 0, 'calls': 0}

    def _accumulate(data: dict):
        u = data.get('usage') or {}
        usage_total['prompt_tokens'] += u.get('prompt_tokens', 0) or 0
        usage_total['completion_tokens'] += u.get('completion_tokens', 0) or 0
        usage_total['total_tokens'] += u.get('total_tokens', 0) or 0
        usage_total['calls'] += 1
        log(f"  usage llamada #{usage_total['calls']}: {u}")

    total_iters = MAX_TOOL_ITERATIONS if use_tools else 1
    for i in range(total_iters):
        if task_id:
            emit_event(task_id, exec_id, 'run', f'iteración {i + 1}/{total_iters} — OpenCode ({model_id})')
        data = call_opencode_once(api_url, model_id, messages, max_tokens, timeout_s, ALL_TOOL_SCHEMAS if use_tools else None, session_id=session_id)
        _accumulate(data)
        choice = (data.get('choices') or [{}])[0]
        message = choice.get('message', {})
        tool_calls = message.get('tool_calls')
        finish_reason = choice.get('finish_reason')

        if not tool_calls:
            content = strip_reasoning_tags(message.get('content') or '')
            # BUG REAL encontrado en produccion, en 2 variantes del mismo
            # problema de fondo: el modelo se queda sin presupuesto de
            # max_tokens ANTES de terminar la respuesta real.
            #   Variante 1 (vista primero, 2 tasks LLM): el contenido sale
            #   TOTALMENTE VACIO porque el presupuesto entero se fue en
            #   razonamiento interno sin llegar a escribir nada visible.
            #   Variante 2 (vista despues, "One-Pager & Private Landing"):
            #   el contenido sale PARCIAL — arranca bien pero se corta a
            #   mitad de una oracion/tabla porque el limite se alcanzo a
            #   mitad de la respuesta visible, no durante el razonamiento.
            # Las dos comparten la misma señal real: finish_reason='length'
            # (la API confirma que corto por limite, no porque el modelo
            # termino de responder). El fix original solo cubria la
            # variante 1 (chequeando ademas "contenido vacio"), dejando
            # pasar la variante 2 como si fuera una respuesta completa y
            # corta — el verificador la rechazaba correctamente por
            # "cortada abruptamente", pero la causa real (nuestro limite de
            # tokens, no una falla del modelo) quedaba invisible. Ahora se
            # reintenta ante CUALQUIER finish_reason='length', pidiendole
            # que continue/complete en vez de asumir que esta vacio.
            if finish_reason == 'length':
                was_empty = not content.strip()
                log(f'ADVERTENCIA: respuesta truncada por limite de tokens (finish_reason=length, {"vacia" if was_empty else "parcial"}) — reintentando UNA vez')
                if was_empty:
                    nudge = (
                        'Tu respuesta anterior se cortó sin nada de contenido visible porque gastaste '
                        'todo el presupuesto de tokens razonando internamente antes de escribir la '
                        'respuesta. Esta vez respondé DIRECTO: anda derecho al resultado final pedido, '
                        'sin explicar tu razonamiento paso a paso.'
                    )
                else:
                    nudge = (
                        f'Tu respuesta anterior se cortó a mitad de camino por límite de tokens '
                        f'(llegaste hasta: "...{content[-200:]}"). Reescribí la respuesta completa desde '
                        f'el principio, priorizando terminarla entera aunque sea más breve/concisa en '
                        f'cada sección — es mejor una respuesta completa y compacta que una detallada '
                        f'pero cortada a la mitad.'
                    )
                direct_messages = messages + [{'role': 'user', 'content': nudge}]
                try:
                    retry_data = call_opencode_once(api_url, model_id, direct_messages, max_tokens, timeout_s, ALL_TOOL_SCHEMAS if use_tools else None, session_id=session_id)
                    _accumulate(retry_data)
                    retry_content = (retry_data.get('choices') or [{}])[0].get('message', {}).get('content') or ''
                    retry_finish = (retry_data.get('choices') or [{}])[0].get('finish_reason')
                    # Solo reemplazar si el reintento realmente trajo algo
                    # (o si el original estaba vacio, cualquier cosa es mejor).
                    if retry_content.strip() or was_empty:
                        content = strip_reasoning_tags(retry_content)
                    if retry_finish == 'length':
                        log('ADVERTENCIA: el reintento tambien se corto por limite de tokens — se acepta tal cual, no se reintenta una segunda vez')
                except Exception as e:
                    log(f'ADVERTENCIA: fallo el reintento tras truncamiento: {e}')
            # BUG REAL encontrado en produccion (visto en vivo con 2 tasks
            # reales, "Demo Environment & Presales Script" y "LLM
            # Integration for Triage & Drafting"): el resumen final se
            # cortaba en seco a los 6000 caracteres, y para tareas con mucho
            # contenido real (varios archivos, guiones largos) el corte caia
            # literalmente a mitad de una oracion — el verificador entonces
            # rechazaba un trabajo real y completo por parecer "incompleto",
            # cuando en realidad el corte era artificial nuestro, no del
            # modelo. Subido a 16000.
            return (content or '(sin output)')[:16000], tool_log, usage_total

        messages.append(message)
        for tc in tool_calls:
            fn = tc.get('function', {})
            name = fn.get('name', '')
            try:
                args = json.loads(fn.get('arguments') or '{}')
            except json.JSONDecodeError:
                args = {}
            result = code_index.execute_tool(name, args) if name in CODE_INDEX_TOOL_NAMES else file_tools.execute_tool(name, args)
            tool_log.append({'tool': name, 'args': args, 'resultPreview': result[:200]})
            if task_id and name in ('write_file', 'edit_file', 'delete_file', 'move_file') and not result.startswith('ERROR'):
                verbo = {'write_file': 'escribió', 'edit_file': 'editó', 'delete_file': 'borró', 'move_file': 'movió'}[name]
                destino = f'{args.get("rel_from", "?")} → {args.get("rel_to", "?")}' if name == 'move_file' else args.get('rel_path', '?')
                emit_event(task_id, exec_id, 'write', f'{verbo} {destino}')
            messages.append({
                'role': 'tool',
                'tool_call_id': tc.get('id', ''),
                'content': result[:4000],
            })

        # Aviso temprano (mitigacion, no el fix de raiz): un paso antes del
        # ultimo, avisarle explicitamente que se le acaba el presupuesto —
        # asi cierra con lo que tenga en vez de arrancar una exploracion
        # nueva que no va a poder terminar.
        if use_tools and i == MAX_TOOL_ITERATIONS - 2:
            messages.append({
                'role': 'user',
                'content': (
                    'Te queda 1 sola llamada a herramientas antes del limite. '
                    'Si ya tenes lo suficiente, respondé AHORA con el resumen final en texto plano '
                    '(sin llamar a mas herramientas). Si de verdad necesitas un paso mas, usalo para '
                    'algo imprescindible y despues respondé con el resumen final aunque quede incompleto.'
                ),
            })

    # FIX DE RAIZ (generaliza a CUALQUIER tarea, no solo esta): antes, si el
    # modelo llegaba al techo de iteraciones todavia pidiendo herramientas
    # (ej. justo la ronda en que escribio un archivo real con write_file),
    # la funcion devolvia un texto de relleno ("se alcanzo el limite...") en
    # vez de lo que el modelo realmente hizo. Ese texto de relleno es lo
    # unico que ve el verificador del lado del servidor — no ve el codigo
    # real escrito — asi que siempre marcaba la tarea FAILED, y
    # finalizeExecution descartaba el worktree entero (con el archivo real
    # ya escrito adentro). Visto en vivo con "OAuth2 M365 & MSAL
    # Integration": el paso 34/34 fue un write_file real de un cliente MSAL
    # completo y correcto, tirado a la basura por esto.
    #
    # Fix: agotado el presupuesto de tool-calling, se hace UNA llamada mas
    # SIN herramientas (tools=None), forzando al modelo a devolver texto
    # plano resumiendo TODO lo que realmente hizo hasta aca (incluyendo el
    # historial completo de tool calls/resultados que ya tiene en el
    # contexto). Asi el verificador siempre recibe informacion real del
    # trabajo hecho, nunca un placeholder — y puede juzgar correctamente si
    # lo que se alcanzo a hacer alcanza o no, en vez de que se pierda por un
    # problema de contabilidad del loop.
    if use_tools:
        messages.append({
            'role': 'user',
            'content': (
                'Se acabaron los pasos de herramientas disponibles. No llames a mas herramientas. '
                'Respondé AHORA, en texto plano, con un resumen honesto y especifico de todo lo que '
                'lograste hacer realmente (que archivos creaste o modificaste, que quedo pendiente). '
                'Si no alcanzaste a completar la tarea, decilo explicitamente en vez de simular que '
                'terminaste. Si tomaste alguna decision de diseño/convencion relevante para otras '
                'tareas de este mismo sprint, agregala al final bajo "DECISIONES:" (o "DECISIONES: '
                'ninguna" si no aplica).'
            ),
        })
        try:
            data = call_opencode_once(api_url, model_id, messages, max_tokens, timeout_s, tools=None, session_id=session_id)
            _accumulate(data)
            content = (data.get('choices') or [{}])[0].get('message', {}).get('content') or '(sin output)'
            return strip_reasoning_tags(content)[:16000], tool_log, usage_total
        except Exception as e:
            log(f'ADVERTENCIA: fallo el resumen final forzado: {e}')

    return '(se alcanzo el limite de pasos de herramientas sin una respuesta final)', tool_log, usage_total


def report_completion(task_id: str, exec_id: str, status: str, result_summary: str, duration_ms: int, context_used: str = '', tool_log=None, usage=None):
    headers = {'Content-Type': 'application/json'}
    if INTERNAL_API_KEY:
        headers['X-Api-Key'] = INTERNAL_API_KEY
    resp = requests.post(
        CALLBACK_URL,
        headers=headers,
        json={
            'taskId': task_id,
            'execId': exec_id,
            'status': status,
            'resultSummary': result_summary,
            'durationMs': duration_ms,
            'contextUsed': context_used,
            'toolLog': tool_log or [],
            # Uso real de tokens (suma de todas las llamadas al modelo de esta tarea) y modelo: el Motor lo
            # guarda en TaskExecution.artifacts.usage para poder medir cuanto cuesta cada tarea.
            **({'usage': usage} if usage else {}),
        },
        # 120s: el callback corre finalizeExecution de forma sincrona en el
        # servidor (incluye una llamada real al verificador Sigma), asi que
        # puede tardar bastante mas que un POST comun. Con 30s se vio un caso
        # real donde el servidor SI termino y guardo DONE, pero el cliente ya
        # habia tirado un timeout y mandaba un segundo reporte FAILED que
        # sobreescribia el DONE recien guardado.
        # 420s (antes 120): el cierre incluye tsc (hasta 180s) y el verificador con hasta 2 intentos de 100s.
        timeout=420,
        allow_redirects=False,
    )
    if not resp.ok:
        log(f'ADVERTENCIA: callback fallo ({resp.status_code}): {resp.text[:300]}')


def report_investigation_completion(callback_url: str, task_id: str, exec_id: str, status: str, result_summary: str, duration_ms: int, tool_log=None):
    headers = {'Content-Type': 'application/json'}
    if INTERNAL_API_KEY:
        headers['X-Api-Key'] = INTERNAL_API_KEY
    resp = requests.post(
        callback_url,
        headers=headers,
        json={
            'taskId': task_id, 'execId': exec_id, 'status': status,
            'resultSummary': result_summary, 'durationMs': duration_ms,
            'toolLog': tool_log or [],
        },
        timeout=30, allow_redirects=False,
    )
    if not resp.ok:
        log(f'ADVERTENCIA: callback de investigacion fallo ({resp.status_code}): {resp.text[:300]}')


def process_investigation_task(payload: dict, label: str, callback_url: str):
    """
    Mini-agentes de solo-lectura ("Explicar" y "Proponer plan"): investigan
    el repo REAL con las mismas herramientas completas que un agente CODE
    (ALL_TOOL_SCHEMAS — read_file/write_file/grep_files/list_files/
    find_symbol/get_file_summary/get_dependents), para que el usuario no
    tenga que interpretar el error crudo el mismo, o para proponerle un plan
    de remediacion desglosado antes de tocar nada. A diferencia de una tarea
    CODE real: nunca crean worktree (leen directo del repo en su estado
    actual), y su resultado nunca pasa por finalizeExecution ni toca
    BacklogItem.status — eso lo hace, si corresponde, "Ejecutar plan"
    (dispatchTask real, ver taskDispatcher.ts) una vez que el usuario revisa
    y aprueba lo que este agente propuso.
    """
    start = time.time()
    task_id = payload['taskId']
    exec_id = payload['execId']
    repo_path = payload.get('repoPath')
    log(f'{label} task {task_id[:8]}...')

    # MASD-0015-0001-004: estas tareas (Explicar/Proponer plan/QA) NUNCA usan worktree — leen
    # el repo en su estado actual a proposito (ver docstring de arriba). _traducir_ruta_masd
    # ahora sabe mapear /root/portal-architechia a su bind mount de solo lectura en /opt/masd,
    # asi que esto deja de fallar en silencio.
    repo_root = _traducir_ruta_masd(repo_path or os.environ.get('MASD_REPO_ROOT', '/root/portal-architechia'))
    file_tools.set_repo_root(repo_root)
    code_index.set_repo_root(repo_root)

    try:
        # strategy='CODE' es lo que le da a run_agent acceso a
        # ALL_TOOL_SCHEMAS — con todas las herramientas, tal como pidio el
        # usuario, aunque el system prompt (armado en el portal) le indique
        # que su tarea es investigar/proponer, no modificar codigo todavia.
        result_summary, tool_log, usage = run_agent(
            payload['apiUrl'], payload['modelId'],
            payload['systemPrompt'], payload['userPrompt'], strategy='CODE',
            task_id=task_id, exec_id=exec_id,
        )
        duration_ms = int((time.time() - start) * 1000)
        report_investigation_completion(callback_url, task_id, exec_id, 'DONE', result_summary, duration_ms, tool_log)
        log(f'{label} OK ({duration_ms}ms, {len(tool_log)} llamadas a herramientas) — tokens: {usage["total_tokens"]}')
    except Exception as e:
        duration_ms = int((time.time() - start) * 1000)
        error_msg = f'Error: {e}'[:2000]
        log(f'{label} FALLO: {error_msg}')
        try:
            report_investigation_completion(callback_url, task_id, exec_id, 'FAILED', error_msg, duration_ms)
        except Exception as cb_err:
            log(f'ADVERTENCIA: no se pudo reportar el fallo de {label} al portal: {cb_err}')


# Bind mounts que le dan a "masd" (usuario sin privilegios que corre este worker, ver
# MASD-0023-0006 items 1/2) una ruta de entrada a /root/worktrees y /root/repos sin darle
# acceso a /root en si (que tiene otros proyectos con secretos que masd no debe poder leer).
_MASD_PATH_MAP = [
    ('/root/worktrees/', '/opt/masd/worktrees/'),
    ('/root/repos/', '/opt/masd/repos/'),
    # MASD-0015-0001-004: faltaba este — el repo principal del portal NUNCA tuvo bind mount,
    # asi que cualquier tarea de solo-lectura (Explicar/Proponer plan/QA) que cae al default de
    # MASD_REPO_ROOT (sin worktree) no podia leer nada: list_files/read_file/grep_files fallaban
    # en silencio por permission denied y devolvian vacio, no un error explicito — asi se
    # disfrazo de "bug de grep_files" cuando en realidad era que "masd" no podia ni entrar a
    # /root. Bind mount de SOLO LECTURA (nunca escritura — este repo es el portal corriendo en
    # vivo, no un worktree de tarea). Sin barra final a proposito: cubre tanto la ruta pelada
    # (el default literal de MASD_REPO_ROOT) como cualquier subruta.
    ('/root/portal-architechia', '/opt/masd/portal-architechia'),
]


def _traducir_ruta_masd(path):
    for viejo, nuevo in _MASD_PATH_MAP:
        if path.startswith(viejo):
            return nuevo + path[len(viejo):]
    return path


# El dispatcher (Node, root) crea node_modules del worktree como un symlink a una ruta
# ABSOLUTA /root/repos/<x>/node_modules o /root/portal-architechia/node_modules (ver
# createTaskWorktree en gitWorktree.ts) — inalcanzable para "masd". Se reescribe el symlink
# para que apunte a la ruta bind-mounteada equivalente ANTES de correr run_command/tsc, que
# son los que necesitan resolver node_modules de verdad.
def _arreglar_symlink_node_modules(repo_root):
    nm = os.path.join(repo_root, 'node_modules')
    if not os.path.islink(nm):
        return
    try:
        destino = os.readlink(nm)
    except OSError:
        return
    nuevo_destino = _traducir_ruta_masd(destino)
    if nuevo_destino != destino:
        try:
            os.unlink(nm)
            os.symlink(nuevo_destino, nm)
        except OSError as e:
            log(f'ADVERTENCIA: no se pudo reescribir el symlink de node_modules ({nm} -> {destino}): {e}')


def process_task(h: Harness, task):
    payload = task.payload
    if task.type == 'explain_task':
        process_investigation_task(payload, 'Explicando', EXPLAIN_CALLBACK_URL)
        h.complete(task, 'explicacion procesada')
        return
    if task.type == 'plan_task':
        process_investigation_task(payload, 'Proponiendo plan de', PLAN_CALLBACK_URL)
        h.complete(task, 'plan procesado')
        return
    if task.type != 'masd_task':
        h.fail(task, 'tipo de tarea no manejado por masd_worker', retry=True)
        return

    start = time.time()
    task_id = payload['taskId']
    exec_id = payload['execId']
    strategy = payload['strategy']
    repo_path = payload.get('repoPath')
    log(f'Procesando task {task_id[:8]}... (agente: {task.agent}, strategy: {strategy})')

    # Si el dispatcher armo un worktree aislado para esta tarea CODE (dentro
    # de un sprint), leer/escribir ahi — nunca en el repo principal donde
    # corre el servidor Next.js en vivo. Si no vino repoPath (tarea LLM, o
    # CODE sin sprint), se usa el default (MASD_REPO_ROOT / repo principal).
    repo_path = _traducir_ruta_masd(repo_path) if repo_path else None
    if repo_path:
        _arreglar_symlink_node_modules(repo_path)
        file_tools.set_repo_root(repo_path)
        code_index.set_repo_root(repo_path)
    else:
        # Sin worktree (tarea LLM, o CODE sin sprint): cae al repo principal del portal.
        # MASD-0015-0001-004: ahora SI hay bind mount para el repo principal, pero de SOLO
        # LECTURA (ver _MASD_PATH_MAP) — asi que esto arregla la lectura (list_files/read_file/
        # grep_files) sin reabrir el riesgo original: escribir codigo real fuera de un worktree
        # aislado sigue sin poder pasar, write_file va a fallar igual por el mount read-only.
        repo_root = _traducir_ruta_masd(os.environ.get('MASD_REPO_ROOT', '/root/portal-architechia'))
        file_tools.set_repo_root(repo_root)
        code_index.set_repo_root(repo_root)

    try:
        result_summary, tool_log, usage = run_agent(
            payload['apiUrl'], payload['modelId'],
            payload['systemPrompt'], payload['userPrompt'], strategy,
            task_id=task_id, exec_id=exec_id,
        )
        duration_ms = int((time.time() - start) * 1000)
        report_completion(task_id, exec_id, 'DONE', result_summary, duration_ms, payload.get('contextPreview', ''), tool_log,
                          usage={**usage, 'model': payload.get('modelId')})
        h.complete(task, result_summary)
        tools_used = f', {len(tool_log)} llamadas a herramientas' if tool_log else ''
        log(f'OK ({duration_ms}ms{tools_used}) — tokens: {usage["total_tokens"]} '
            f'({usage["prompt_tokens"]} prompt + {usage["completion_tokens"]} completion, '
            f'{usage["calls"]} llamada(s) a OpenCode)')
    except Exception as e:
        duration_ms = int((time.time() - start) * 1000)
        error_msg = f'Error: {e}'[:2000]
        log(f'FALLO: {error_msg}')
        # MASD-0015-0001-007: antes esto siempre reportaba FAILED al portal ANTES de saber si
        # el Harness iba a reintentar — el usuario veia la tarea como terminalmente fallida (y
        # recibia la alerta de error) en el primer intento, aunque atras el Harness fuera a
        # reintentar solo en segundo plano. Efecto real: parecia que "el retry automatico nunca
        # se disparaba", cuando en realidad SI se disparaba (h.fail() ya reencolaba bien) pero
        # de forma completamente invisible para quien miraba el portal — y si la persona, viendo
        # FAILED, reseteaba y redisparaba a mano antes de que el reintento automatico corriera,
        # terminaban DOS ejecuciones reales para la misma tarea.
        # Ahora: mientras queden reintentos (chequeado ANTES de que h.fail() los consuma), se
        # narra el intento fallido como EVENTO de traza (visible, pero no terminal — la tarea
        # sigue IN_PROGRESS de verdad) y NO se le reporta FAILED al portal. Recien cuando se
        # agotan los reintentos se reporta el estado terminal.
        quedan_reintentos = task.retries < task.max_retries
        if quedan_reintentos:
            emit_event(task_id, exec_id, 'info',
                       f'intento {task.retries + 1}/{task.max_retries + 1} falló ({error_msg[:300]}) — reintentando automáticamente')
        else:
            try:
                report_completion(task_id, exec_id, 'FAILED', error_msg, duration_ms)
            except Exception as cb_err:
                log(f'ADVERTENCIA: no se pudo reportar el fallo al portal: {cb_err}')
        h.fail(task, str(e))


def main_loop():
    h = Harness()
    log(f'Iniciado. Backend Harness: {h.backend_type}. Repo: {file_tools.REPO_ROOT}')
    while True:
        try:
            task = h.next_task(WORKER_ID)
            if task is None:
                time.sleep(POLL_INTERVAL_S)
                continue
            process_task(h, task)
        except Exception as e:
            log(f'Error en el loop principal: {e}')
            time.sleep(POLL_INTERVAL_S)


if __name__ == '__main__':
    main_loop()
