# Home Monitoring Stack — Config Adaptations for Pelagos

The stack was originally designed for Kubernetes + kube-prometheus-stack (Helm).
The `monitoring-setup/` directory contains the original Helm charts. The
`pelagos/` directory contains the rewritten config that runs under
`pelagos compose`. This document records what changed and why.

---

## What didn't change

Most config files are identical to what you'd write for plain Docker Compose:
`prometheus.yml`, `alertmanager.yml`, `graphite_mapping.yaml`, and the Grafana
datasource provisioning YAML are standard upstream formats that didn't need
modification for Pelagos. Only two things required deliberate adaptation.

---

## `prometheus.yml` — self-scrape target

**Original (Kubernetes / Docker Compose idiom):**
```yaml
- job_name: prometheus
  static_configs:
    - targets: ['localhost:9090']
```

**Pelagos version:**
```yaml
- job_name: prometheus
  static_configs:
    - targets: ['prometheus:9090']
```

**Why:** Inside a Pelagos container, `localhost` is not a hostname — it's the
loopback address of the container itself. Prometheus is a separate container, so
`localhost:9090` reaches nothing. Pelagos's built-in DNS service discovery
registers each service under its compose service name, so `prometheus` resolves
to the correct container IP on the `monitoring` bridge network. All other
scrape targets in `prometheus.yml` already used service names (`snmp-exporter`,
`mktxp`, etc.) for the same reason.

**Lesson:** Never use `localhost` to refer to a sibling service. Always use the
service name.

---

## `compose.reml` — Prometheus startup flags

**Added flag:**
```
--web.enable-lifecycle
```

**Why:** Without this flag, `POST http://localhost:9090/-/reload` returns
`Lifecycle API is not enabled`. The flag enables the hot-reload endpoint so
`prometheus.yml` changes can be applied without restarting the stack:

```bash
curl -X POST http://localhost:9090/-/reload
```

This is also common practice in Docker Compose deployments but easy to forget.

---

## mktxp — writable config directory

mktxp needs a *writable* directory at startup: it writes state files alongside
its config. The config file is read-only source-controlled content. Mounting the
same directory both read-only (to protect the file) and read-write (for state)
is a contradiction.

**Solution:** bind-mount the config file read-only at a staging path, then use a
`tmpfs` for the working directory and a shell wrapper to copy it in at startup:

```
(bind-mount "./config/mktxp/mktxp.conf" "/conf/mktxp.conf" :ro)
(tmpfs "/config")
(command "sh" "-c" "cp /conf/mktxp.conf /config/mktxp.conf && mktxp --cfg-dir /config export")
```

This pattern — staging a read-only file into a tmpfs — is the idiomatic Pelagos
approach for any service that mutates its own config directory.

---

## mktxp — user resolution

mktxp's image declares `USER mktxp` in its Dockerfile. Pelagos (like Docker)
applies the image's default user when starting the container. But `mktxp` is an
image-internal user defined in the image's `/etc/passwd` — it doesn't exist on
the host. Pelagos's original user resolution looked up usernames in the *host*
`/etc/passwd`, causing a startup failure.

**Fix (in Pelagos itself):** `parse_user_in_layers()` reads `/etc/passwd` from
the image layers before falling back to the host. This mirrors Docker's
behaviour and is required for any image that defines its own non-root user.

---

## snmp.yml — module format for v0.21.0

snmp-exporter v0.21.0 expects module names at the **top level** of `snmp.yml`
with no `modules:` wrapper:

```yaml
mikrotik:          # correct for v0.21.0
  walk: [...]
  auth: ...
```

Not:
```yaml
modules:           # wrong — this is the newer format
  mikrotik:
    walk: [...]
```

The Prometheus scrape URL uses `?module=mikrotik` as a query parameter, so the
top-level key must match exactly.

---

## snmp_mikrotik — scrape relabelling

The SNMP exporter is a proxy: Prometheus doesn't scrape the router directly, it
scrapes the exporter and tells it which router to walk. This requires a
`relabel_configs` block that rewrites the scrape request on the fly:

```yaml
- job_name: snmp_mikrotik
  static_configs:
    - targets: ['192.168.88.1']   # the router
  metrics_path: /snmp
  params:
    module: [mikrotik]
  relabel_configs:
    - source_labels: [__address__]
      target_label: __param_target   # pass router IP as ?target=
    - source_labels: [__param_target]
      target_label: instance         # label metrics with the router IP
    - target_label: __address__
      replacement: snmp-exporter:9116  # but actually HTTP-connect here
```

This is standard snmp-exporter configuration; it works identically in Pelagos,
Docker Compose, and Kubernetes.

---

## graphite_mapping.yaml — collectd metric name translation

TrueNAS pushes metrics in collectd's Graphite wire format:
```
servers.truenas.disk-sda.disk_io.read  1234  1700000000
```

Prometheus expects structured labels, not dot-separated names. The mapping file
contains regex rules that parse the dot-separated path into a metric name and
label set:

```yaml
- match: 'servers\.(.*)\.disk-(.*)\.(.*)\.(.*)'
  name: 'truenas_${3}_${4}'
  labels:
    hostname: ${1}
    device: ${2}
```

Result: `truenas_disk_io_read{hostname="truenas", device="sda"}`.

The mapping covers interfaces, datasets, memory, ZFS ARC, processes, load,
swap, uptime, CPU, and NFS. It is a verbatim translation of the original Helm
values from `monitoring-setup/truenas-graphite-exporter/values.yaml`.

---

## truenas-api-exporter — locally built image

The TrueNAS API exporter is not published to any registry. It is built from a
`Remfile` in `monitoring-setup/truenas-graphite-exporter/`:

```
FROM python:3.11-slim
WORKDIR /app
RUN pip install --no-cache-dir prometheus-client==0.19.0 requests==2.31.0
COPY truenas_api_exporter.py .
CMD ["python", "-u", "/app/truenas_api_exporter.py"]
```

`start-monitoring.sh` checks for the image with `pelagos image ls` and builds it
if absent. Three bugs in Pelagos's build engine were uncovered and fixed during
this process:

1. **EINVAL on overlay mount** — Debian-based images (python:3.11-slim is
   Debian) have repeated layer digests for empty marker layers. Pelagos's
   `execute_run()` didn't deduplicate `lowerdir` entries; overlayfs returns
   EINVAL for duplicates. Fixed with HashSet deduplication.

2. **WORKDIR didn't create the directory** — `WORKDIR /app` only set
   `config.working_dir`; it never materialised `/app` in the layer. The
   subsequent `RUN` step would fail when trying to `chdir` into a
   non-existent directory. Fixed with `execute_workdir()`.

3. **`COPY file.py .` → EISDIR** — The destination `.` resolved to the temp
   layer directory itself; `fs::copy(file, directory)` returns EISDIR. Docker's
   semantics require `.` to mean "copy into WORKDIR keeping the filename".
   Fixed with `resolve_copy_dest()` which threads `working_dir` through all
   COPY/ADD code paths.
