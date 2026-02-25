# Jupyter Developer Stack

A JupyterLab + Redis stack for interactive data science and notebook development.
Redis provides a shared in-memory cache accessible from notebook code.

## Architecture

```
jupyter-net (10.89.0.0/24):
  jupyterlab  ← port 8888 → host
  redis       ← internal only
```

| Service | Image | Role |
|---------|-------|------|
| **redis** | `jupyter-redis:latest` | In-memory cache; notebooks store/retrieve computed results |
| **jupyterlab** | `jupyter-jupyterlab:latest` | JupyterLab server with scientific Python stack |

Dependencies: `jupyterlab` waits for `redis` TCP port 6379 before starting.

## Scientific Python Stack

The `jupyterlab` image installs these packages via Alpine APK (no glibc required):

| Package | Source |
|---------|--------|
| `python3`, `py3-pip` | APK |
| `py3-numpy`, `py3-pandas` | APK |
| `py3-matplotlib`, `py3-scipy` | APK |
| `py3-scikit-learn` | APK |
| `jupyterlab`, `ipykernel`, `redis` | pip |

## What `compose.reml` Demonstrates

### `define` — named configuration

```lisp
(define mem-redis   "64m")
(define mem-jupyter "512m")
(define cpu-jupyter "1.0")
```

All resource limits in one place; no magic numbers inside service blocks.

### `env` — port override without file editing

```lisp
(define jupyter-port
  (let ((p (env "JUPYTER_PORT")))
    (if (null? p) 8888 (string->number p))))
```

```bash
# Run on a non-default port
JUPYTER_PORT=9999 sudo remora compose up -f compose.reml -p jupyter
```

### `on-ready` — observable readiness

```lisp
(on-ready "redis"
  (lambda ()
    (log "redis: cache layer ready — JupyterLab kernel can connect")))
```

Fires after redis's TCP health check passes, before JupyterLab starts.
The log message confirms the notebook kernel will find redis on the first connection.

## Running

```bash
# Build remora first
cargo build --release
export PATH=$PWD/target/release:$PATH

# Run the stack (requires root)
sudo ./examples/compose/jupyter/run.sh

# Open in browser
open http://localhost:8888/lab
```

The script:
1. Pulls `alpine:latest` and builds both images
2. Runs `remora compose up -f compose.reml -p jupyter` in foreground
3. Waits for JupyterLab to accept connections on port 8888
4. Runs 4 smoke tests (API, UI, kernel specs, sessions)
5. Tears down with `remora compose down -v`

## Using Redis from Notebooks

The Redis host is injected as `REDIS_HOST=redis` and `REDIS_PORT=6379`.
In a notebook cell:

```python
import os, redis

r = redis.Redis(
    host=os.environ["REDIS_HOST"],
    port=int(os.environ["REDIS_PORT"]),
    decode_responses=True,
)

# Cache an expensive computation
r.set("result:v1", "42")
print(r.get("result:v1"))  # → "42"
```

## Named Volume

Notebooks are stored in the `jupyter-notebooks` named volume mounted at `/notebooks`.
They survive container restarts and `compose down` (but not `compose down -v`).
