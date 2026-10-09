"""
harness_api.py — API de monitoreo del Harness
Puerto: 8767
Dashboard de estado de colas y tareas en tiempo real
"""
import json
from pathlib import Path
from fastapi import FastAPI, HTTPException
from fastapi.middleware.cors import CORSMiddleware
import sys
sys.path.insert(0, '/root')
from harness import Harness, Task, TaskStatus, TaskPriority, QUEUE_DIR, REDIS_AVAILABLE

app = FastAPI(title="Harness Monitor", description="Dashboard de colas del Dev Engine SAGE", version="1.0.0")
app.add_middleware(CORSMiddleware, allow_origins=["*"], allow_methods=["*"], allow_headers=["*"])

DLQ_DIR = QUEUE_DIR / "dlq"
DLQ_DIR.mkdir(exist_ok=True)


def get_redis():
    import redis
    from harness import REDIS_HOST, REDIS_PORT
    return redis.Redis(host=REDIS_HOST, port=REDIS_PORT, decode_responses=True, socket_connect_timeout=3)


@app.get("/")
def health():
    h = Harness()
    return {"status": "ok", "backend": h.backend_type, "version": "1.0.0"}


@app.get("/queues")
def queue_status():
    """Estado de las colas por prioridad (Redis o JSON)."""
    if REDIS_AVAILABLE:
        r = get_redis()
        result = {}
        for priority in ["HIGH", "MEDIUM", "LOW"]:
            key = f"harness:queue:{priority}"
            result[priority] = r.llen(key)
        # Dead-letter queue en Redis
        result["DLQ"] = r.llen("harness:dlq")
        return {"backend": "redis", "queues": result}
    else:
        pending = list((QUEUE_DIR / "pending").iterdir()) if (QUEUE_DIR / "pending").exists() else []
        dlq = list(DLQ_DIR.iterdir()) if DLQ_DIR.exists() else []
        counts = {"HIGH": 0, "MEDIUM": 0, "LOW": 0, "DLQ": len(dlq)}
        for f in pending:
            for p in ["HIGH", "MEDIUM", "LOW"]:
                if f.name.startswith(p + "_"):
                    counts[p] += 1
        return {"backend": "json", "queues": counts}


@app.get("/tasks")
def list_tasks(status: str = None, limit: int = 20):
    """Lista tareas recientes, opcionalmente filtradas por status."""
    tasks_dir = QUEUE_DIR / "tasks"
    if not tasks_dir.exists():
        return {"tasks": [], "total": 0}
    files = sorted(tasks_dir.glob("*.json"), key=lambda f: f.stat().st_mtime, reverse=True)
    tasks = []
    for f in files[:100]:
        try:
            d = json.loads(f.read_text())
            if status and d.get("status") != status:
                continue
            tasks.append({
                "id": d["id"][:8] + "...",
                "type": d["type"],
                "agent": d["agent"],
                "status": d["status"],
                "priority": d["priority"],
                "retries": d.get("retries", 0),
                "created_at": d.get("created_at", "")[:19],
                "completed_at": (d.get("completed_at") or "")[:19],
                "error": d.get("error"),
            })
        except Exception:
            continue
        if len(tasks) >= limit:
            break
    return {"tasks": tasks, "total": len(tasks), "backend": "redis" if REDIS_AVAILABLE else "json"}


@app.get("/tasks/{task_id}")
def get_task(task_id: str):
    """Detalle completo de una tarea por su ID."""
    h = Harness()
    task = h.get_task(task_id)
    if not task:
        raise HTTPException(status_code=404, detail="Tarea no encontrada")
    return task.to_dict()


@app.get("/dlq")
def dead_letter_queue():
    """Lista de tareas en la dead-letter queue (fallidas sin más reintentos)."""
    if REDIS_AVAILABLE:
        r = get_redis()
        items = r.lrange("harness:dlq", 0, 50)
        tasks = []
        for tid in items:
            data = r.get(f"harness:task:{tid}")
            if data:
                tasks.append(json.loads(data))
        return {"dlq": tasks, "count": len(tasks), "backend": "redis"}
    else:
        tasks = []
        for f in sorted(DLQ_DIR.glob("*.json"), key=lambda f: f.stat().st_mtime, reverse=True)[:50]:
            tasks.append(json.loads(f.read_text()))
        return {"dlq": tasks, "count": len(tasks), "backend": "json"}


@app.post("/dispatch")
def dispatch_task(payload: dict):
    """Encola una tarea nueva."""
    h = Harness()
    task = Task(
        type=payload.get("type", "generic"),
        agent=payload.get("agent", "atlas"),
        payload=payload.get("payload", {}),
        priority=TaskPriority(payload.get("priority", "MEDIUM")),
    )
    task_id = h.dispatch(task)
    return {"ok": True, "task_id": task_id}
