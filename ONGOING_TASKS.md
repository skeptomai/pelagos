# Ongoing Tasks

## Current State (Feb 23, 2026)

The monitoring stack now has 6 services running (or ready to run):
- snmp-exporter :9116, plex-exporter :9594, mktxp :49090,
  graphite-exporter :9108/:2003, prometheus :9090, grafana :3000

mktxp and graphite-exporter have been added this session and are committed.
The next session should add alertmanager and then build truenas-api-exporter.

---

## Completed This Session

### mktxp (MikroTik RouterOS API exporter) ✅
- `config/mktxp/mktxp.conf` — credentials from monitoring-setup/mktxp/values.yaml
- bind-mounted read-only at `/config/mktxp.conf`, command: `mktxp --cfg-dir /config export`
- Port :49090
- prometheus.yml job: `mktxp` → `mktxp:49090`
- compose.rem: prometheus depends-on `(mktxp :ready-port 49090)`

### graphite-exporter (TrueNAS collectd metrics) ✅
- `config/graphite/graphite_mapping.yaml` — translated from Helm chart configmap
- Listens :2003 for graphite push, exposes :9108/metrics
- mapping translates `servers.<host>.interface-<dev>.if_*` → `truenas_interface_*` etc.
- prometheus.yml job: `truenas_graphite` → `graphite-exporter:9108`
- compose.rem: prometheus depends-on `(graphite-exporter :ready-port 9108)`
- TrueNAS: System → Advanced → Reporting → Graphite, set host IP and port 2003

---

## Completed Previous Session

### Bind-Mount Support in Compose ✅
### Core Monitoring Stack Running ✅ (4-service: snmp, plex, prometheus, grafana)
### Runtime Bugs Fixed ✅
See previous ONGOING_TASKS.md entries for full details.

---

## Next Task: alertmanager + truenas-api-exporter

### alertmanager ✅ (ready to add)

Config template at `monitoring-setup/prometheus/alertmanager-config.yaml.template`:
- Uses Pushover for notifications (user_key + API token in secrets)
- Routes all alerts to `pushover`, suppresses `Watchdog`
- The PUSHOVER credentials are NOT in the monitoring-setup repo (they were in K8s secrets)
  — need to find them or create new ones at https://pushover.net
- For now, can deploy alertmanager with a null receiver (no notifications) and add Pushover later

**alertmanager.yml** to create at `config/alertmanager/alertmanager.yml`:
```yaml
global:
  resolve_timeout: 5m
route:
  group_by: ['alertname']
  group_wait: 10s
  group_interval: 10s
  repeat_interval: 12h
  receiver: 'null'
receivers:
- name: 'null'
```

**prometheus.yml** additions needed:
```yaml
alerting:
  alertmanagers:
    - static_configs:
        - targets: ['alertmanager:9093']
```

**compose.rem** service:
```lisp
(service alertmanager
  (image "prom/alertmanager:latest")
  (network monitoring)
  (port 9093 9093)
  (bind-mount "./config/alertmanager/alertmanager.yml" "/etc/alertmanager/alertmanager.yml" :ro)
  (command
    "/bin/alertmanager"
    "--config.file=/etc/alertmanager/alertmanager.yml"
    "--storage.path=/alertmanager"))
```

prometheus depends-on alertmanager: `(alertmanager :ready-port 9093)`

### truenas-api-exporter (Custom Python image)

Source: `~/Projects/home-monitoring/monitoring-setup/truenas-graphite-exporter/truenas_api_exporter.py`
Also: `Dockerfile.api-exporter` in the same directory (use as Remfile reference)

The exporter:
- Polls TrueNAS REST API at `http://192.168.88.30` for SMART data + ZFS pool health
- Exposes Prometheus metrics on :9100/metrics (check the py script for actual port)
- Needs env vars: `TRUENAS_HOST`, `TRUENAS_API_KEY`, `VERIFY_SSL`
- TrueNAS API key was stored as K8s secret — need to check TrueNAS SCALE UI to get/create one

**Remfile approach:**
```dockerfile
FROM python:3.11-slim
WORKDIR /app
COPY requirements.txt .
RUN pip install -r requirements.txt
COPY truenas_api_exporter.py .
CMD ["python", "truenas_api_exporter.py"]
```

Will need `--network bridge` on RUN steps for pip to reach PyPI.
Build context: `~/Projects/home-monitoring/monitoring-setup/truenas-graphite-exporter/`
Tag: `truenas-api-exporter:latest`

Need to check if requirements.txt exists or infer deps from imports.

---

## Known Limitations / Watch List

- **Symbolic user resolution uses host `/etc/passwd`** — works for standard
  system users (`nobody`, `root`) and numeric IDs. A user defined only inside
  the container image's `/etc/passwd` won't resolve.

- **compose `(command ...)` replaces entire entrypoint+cmd** — if you want to
  pass extra args to the image's existing entrypoint, you must repeat the
  entrypoint in the `(command ...)` list. See prometheus, graphite-exporter,
  alertmanager for the pattern.

- **Plex token** — script reads from `$PLEX_TOKEN` env var or
  `monitoring-setup/.env`. The `YOUR_PLEX_TOKEN_HERE` placeholder in
  compose.rem is substituted at runtime.

- **mktxp writable config dir** — mktxp might try to write state alongside
  the config. If it exits with a write error, add a tmpfs at `/config` and
  use a startup script to copy the bind-mounted conf into it first.

- **TrueNAS graphite push** — must configure TrueNAS to push to this host's
  IP on port 2003. The graphite-exporter service must be reachable from the
  NAS (not just localhost). Port 2003 is host-mapped, so it's accessible.

- **Pushover credentials** — need to retrieve user_key and API token from
  Pushover account before alertmanager can send real notifications.
