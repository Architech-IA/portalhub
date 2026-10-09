"""
harness.py — Orquestador central del Dev Engine SAGE
Cola de tareas con backend configurable: Redis (preferido) o JSON (fallback)

Ciclo de vida de una tarea:
  PENDING → IN_PROGRESS → DONE
                        → FAILED
                        → RETRY (max 3 intentos)

Uso:
    from harness import Harness, Task
    h = Harness()
    task_id = h.dispatch(Task(type="code_review", agent="atlas", payload={"repo": "portal"}))
    result = h.get_task(task_id)
"""
import json
import time
import uuid
import fcntl
from datetime import datetime, timezone
from enum import Enum
from pathlib import Path
from dataclasses import dataclass, field, asdict
from typing import Any, Optional

# ─── Intentar Redis, caer a JSON si no está disponible ────────────────────────

QUEUE_DIR = Path("/root/harness-queue")
QUEUE_DIR.mkdir(exist_ok=True)
(QUEUE_DIR / "tasks").mkdir(exist_ok=True)
(QUEUE_DIR / "pending").mkdir(exist_ok=True)
(QUEUE_DIR / "results").mkdir(exist_ok=True)

REDIS_HOST = "172.18.0.2"  # scheduling-redis Docker container IP
REDIS_PORT = 6379

try:
    import redis as _redis
    _r = _redis.Redis(host=REDIS_HOST, port=REDIS_PORT, decode_responses=True, socket_connect_timeout=3)
    _r.ping()
    REDIS_AVAILABLE = True
except Exception:
    REDIS_AVAILABLE = False


# ─── Contrato de tarea ────────────────────────────────────────────────────────

class TaskStatus(str, Enum):
    PENDING     = "PENDING"
    IN_PROGRESS = "IN_PROGRESS"
    DONE        = "DONE"
    FAILED      = "FAILED"
    RETRY       = "RETRY"


class TaskPriority(str, Enum):
    LOW    = "LOW"
    MEDIUM = "MEDIUM"
    HIGH   = "HIGH"


@dataclass
class Task:
    """Contrato estándar de una tarea del Harness."""
    type: str                          # Tipo de tarea: code_review, memory_read, backlog_update, etc.
    agent: str                         # Slug del agente destino: ares, atlas, iris, orion, vesta
    payload: dict = field(default_factory=dict)  # Datos específicos de la tarea
    priority: TaskPriority = TaskPriority.MEDIUM
    id: str = field(default_factory=lambda: str(uuid.uuid4()))
    status: TaskStatus = TaskStatus.PENDING
    retries: int = 0
    max_retries: int = 3
    created_at: str = field(default_factory=lambda: datetime.now(timezone.utc).isoformat())
    started_at: Optional[str] = None
    completed_at: Optional[str] = None
    result: Optional[Any] = None
    error: Optional[str] = None

    def to_dict(self) -> dict:
        d = asdict(self)
        d["priority"] = self.priority.value
        d["status"] = self.status.value
        return d

    @classmethod
    def from_dict(cls, d: dict) -> "Task":
        d["priority"] = TaskPriority(d.get("priority", "MEDIUM"))
        d["status"] = TaskStatus(d.get("status", "PENDING"))
        return cls(**d)


# ─── Backend: Redis ──────────────────────────────────────────────────────────

class RedisBackend:
    QUEUE_KEY = "harness:queue:{priority}"
    TASK_KEY  = "harness:task:{id}"
    LOCK_KEY  = "harness:lock:{id}"

    def __init__(self):
        import redis
        self.r = redis.Redis(host=REDIS_HOST, port=REDIS_PORT, decode_responses=True, socket_connect_timeout=3)

    def push(self, task: Task):
        key = self.QUEUE_KEY.format(priority=task.priority.value)
        self.r.lpush(key, task.id)
        self.r.set(self.TASK_KEY.format(id=task.id), json.dumps(task.to_dict()))

    def pop(self, agent: str) -> Optional[Task]:
        for priority in [TaskPriority.HIGH, TaskPriority.MEDIUM, TaskPriority.LOW]:
            key = self.QUEUE_KEY.format(priority=priority.value)
            task_id = self.r.rpop(key)
            if task_id:
                data = self.r.get(self.TASK_KEY.format(id=task_id))
                if data:
                    return Task.from_dict(json.loads(data))
        return None

    def update(self, task: Task):
        self.r.set(self.TASK_KEY.format(id=task.id), json.dumps(task.to_dict()))

    def get(self, task_id: str) -> Optional[Task]:
        data = self.r.get(self.TASK_KEY.format(id=task_id))
        return Task.from_dict(json.loads(data)) if data else None

    def acquire_lock(self, task_id: str, ttl: int = 300) -> bool:
        """Lock atómico para evitar doble ejecución (SET NX EX)."""
        key = self.LOCK_KEY.format(id=task_id)
        return bool(self.r.set(key, "1", nx=True, ex=ttl))

    def release_lock(self, task_id: str):
        self.r.delete(self.LOCK_KEY.format(id=task_id))


# ─── Backend: JSON (fallback sin Redis) ──────────────────────────────────────

class JsonBackend:
    """Cola de tareas basada en archivos JSON con fcntl lock para atomicidad."""

    def _task_file(self, task_id: str) -> Path:
        return QUEUE_DIR / "tasks" / f"{task_id}.json"

    def _pending_file(self, task_id: str, priority: str) -> Path:
        return QUEUE_DIR / "pending" / f"{priority}_{task_id}"

    def push(self, task: Task):
        self._task_file(task.id).write_text(json.dumps(task.to_dict(), indent=2))
        self._pending_file(task.id, task.priority.value).touch()

    def pop(self, agent: str) -> Optional[Task]:
        for priority in ["HIGH", "MEDIUM", "LOW"]:
            candidates = sorted(
                (f for f in (QUEUE_DIR / "pending").iterdir() if f.name.startswith(priority + "_")),
                key=lambda f: f.stat().st_ctime
            )
            for marker in candidates:
                task_id = marker.name[len(priority) + 1:]
                lock_path = QUEUE_DIR / f".lock_{task_id}"
                if lock_path.exists():
                    continue  # Ya está siendo procesada
                marker.unlink(missing_ok=True)
                task_file = self._task_file(task_id)
                if task_file.exists():
                    return Task.from_dict(json.loads(task_file.read_text()))
        return None

    def update(self, task: Task):
        self._task_file(task.id).write_text(json.dumps(task.to_dict(), indent=2))

    def get(self, task_id: str) -> Optional[Task]:
        f = self._task_file(task_id)
        return Task.from_dict(json.loads(f.read_text())) if f.exists() else None

    def acquire_lock(self, task_id: str, ttl: int = 300) -> bool:
        lock_path = QUEUE_DIR / f".lock_{task_id}"
        if lock_path.exists():
            # Verificar si el lock expiró
            age = time.time() - lock_path.stat().st_mtime
            if age < ttl:
                return False
        lock_path.touch()
        return True

    def release_lock(self, task_id: str):
        (QUEUE_DIR / f".lock_{task_id}").unlink(missing_ok=True)


# ─── Harness principal ────────────────────────────────────────────────────────

class Harness:
    """Orquestador central — despacha y gestiona tareas entre agentes SAGE."""

    def __init__(self):
        self.backend = RedisBackend() if REDIS_AVAILABLE else JsonBackend()
        self.backend_type = "redis" if REDIS_AVAILABLE else "json"

    def dispatch(self, task: Task) -> str:
        """Encola una tarea y retorna su id."""
        self.backend.push(task)
        return task.id

    def next_task(self, agent: str) -> Optional[Task]:
        """
        El agente llama a esto para obtener su próxima tarea.
        Aplica lock para evitar doble ejecución.
        """
        task = self.backend.pop(agent)
        if not task:
            return None
        if not self.backend.acquire_lock(task.id):
            return None  # Otra instancia ya la tomó
        task.status = TaskStatus.IN_PROGRESS
        task.started_at = datetime.now(timezone.utc).isoformat()
        self.backend.update(task)
        return task

    def complete(self, task: Task, result: Any = None):
        """Marca la tarea como DONE con su resultado."""
        task.status = TaskStatus.DONE
        task.result = result
        task.completed_at = datetime.now(timezone.utc).isoformat()
        self.backend.update(task)
        self.backend.release_lock(task.id)

    def fail(self, task: Task, error: str, retry: bool = True):
        """Marca la tarea como FAILED o la reencola si quedan reintentos."""
        task.error = error
        if retry and task.retries < task.max_retries:
            task.retries += 1
            task.status = TaskStatus.PENDING
            self.backend.push(task)
        else:
            task.status = TaskStatus.FAILED
            task.completed_at = datetime.now(timezone.utc).isoformat()
            self._send_to_dlq(task)
        self.backend.update(task)
        self.backend.release_lock(task.id)

    def _send_to_dlq(self, task: Task):
        """Envía una tarea fallida a la dead-letter queue."""
        dlq_dir = QUEUE_DIR / "dlq"
        dlq_dir.mkdir(exist_ok=True)
        if REDIS_AVAILABLE and isinstance(self.backend, RedisBackend):
            self.backend.r.lpush("harness:dlq", task.id)
        else:
            (dlq_dir / f"{task.id}.json").write_text(json.dumps(task.to_dict(), indent=2))

    def get_task(self, task_id: str) -> Optional[Task]:
        return self.backend.get(task_id)

    def status(self) -> dict:
        return {
            "backend": self.backend_type,
            "queue_dir": str(QUEUE_DIR),
            "pending": len(list((QUEUE_DIR / "pending").iterdir())) if not REDIS_AVAILABLE else "redis",
        }
