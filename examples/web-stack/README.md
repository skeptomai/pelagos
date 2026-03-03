# Web Stack Example

A 3-container blog application demonstrating Pelagos's multi-container and
multi-network capabilities.

## Architecture

```
frontend (10.88.1.0/24):  proxy ←→ app
backend  (10.88.2.0/24):           app ←→ redis
```

| Container | Networks | Image | Role |
|-----------|----------|-------|------|
| **proxy** | frontend | `web-stack-proxy` | nginx reverse proxy, serves static HTML, forwards `/api/*` |
| **app** | frontend + backend | `web-stack-app` | Python/Bottle REST API for notes CRUD |
| **redis** | backend | `web-stack-redis` | Redis data store |

The proxy and redis containers are on separate networks — they cannot communicate
directly. The app container bridges both networks.

## Features Demonstrated

- **Image build** — `pelagos build` with Remfiles (FROM, RUN, COPY, CMD, ENV, WORKDIR)
- **Multi-network isolation** — frontend and backend networks with per-container attachment
- **Container linking** — `--link name:alias` resolves to the correct IP on a shared network
- **NAT** — outbound internet for `apk add` during builds
- **Bridge IP access** — tests reach nginx via the proxy container's bridge IP
- **Named volumes** — `notes-data` volume created (for demonstration)
- **Network isolation test** — verifies proxy cannot reach redis directly

## Running

```bash
# Build pelagos first
cargo build --release
export PATH=$PWD/target/release:$PATH

# Run the demo (requires root)
sudo ./examples/web-stack/run.sh
```

The script will:
1. Pull `alpine:latest` if needed
2. Build all 3 images
3. Create frontend and backend networks
4. Launch the stack with network isolation
5. Run 6 verification tests (including isolation)
6. Clean up everything on exit

## API Endpoints

| Method | Path | Description |
|--------|------|-------------|
| GET | `/` | Static blog page |
| GET | `/health` | Health check |
| GET | `/api/notes` | List all notes |
| POST | `/api/notes` | Add a note (`{"text": "..."}`) |
| GET | `/api/notes/count` | Note count |
