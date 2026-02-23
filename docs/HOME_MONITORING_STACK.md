# Home Monitoring Stack

A Prometheus + Grafana monitoring stack for home infrastructure, managed by
`remora compose`. Modelled after the Kubernetes/Helm stack at
`~/Projects/home-monitoring`.

## What it monitors

| Target | How | Exporter |
|--------|-----|----------|
| MikroTik router (192.168.88.1) | SNMP | snmp-exporter :9116 |
| MikroTik router (192.168.88.1) | RouterOS API | mktxp :49090 |
| Plex Media Server (192.168.88.30:32400) | REST API | plex-exporter :9594 |
| TrueNAS NAS (192.168.88.30) | Graphite push | graphite-exporter :9108/:2003 |
| All exporters | Prometheus scrape | prometheus :9090 |
| Dashboards | Grafana datasource | grafana :3000 |

## Stack layout

```
~/Projects/home-monitoring/remora/
  compose.rem                          # S-expression compose file
  config/
    prometheus/prometheus.yml          # Scrape targets
    snmp/snmp.yml                      # SNMP module for MikroTik (v0.21.0 format)
    mktxp/mktxp.conf                   # MikroTik RouterOS API credentials
    graphite/graphite_mapping.yaml     # TrueNAS collectd → Prometheus label mapping
    grafana/provisioning/
      datasources/prometheus.yaml      # Auto-provisioned Prometheus datasource
```

## Running the stack

```bash
# Start (background):
sudo -E ~/Projects/remora/scripts/start-monitoring.sh

# Start (foreground, with logs):
sudo -E RUST_LOG=remora=info ~/Projects/remora/scripts/start-monitoring.sh --foreground

# With a Plex token:
sudo -E PLEX_TOKEN=yourtoken ~/Projects/remora/scripts/start-monitoring.sh

# Stop:
sudo -E ~/Projects/remora/scripts/start-monitoring.sh --down

# Stop and wipe grafana data volume:
sudo -E ~/Projects/remora/scripts/start-monitoring.sh --down-volumes
```

The script always rebuilds the remora binary from source first (`cargo build
--release` is a no-op when nothing changed).

## Endpoints

| Service | URL | Credentials |
|---------|-----|-------------|
| Grafana | http://localhost:3000 | admin / prom-operator |
| Prometheus | http://localhost:9090 | — |
| SNMP exporter | http://localhost:9116/metrics | — |
| Plex exporter | http://localhost:9594/metrics | — |
| mktxp | http://localhost:49090/metrics | — |
| Graphite exporter | http://localhost:9108/metrics | — |
| Graphite ingest | tcp://localhost:2003 | — (TrueNAS pushes here) |

## TrueNAS graphite configuration

In TrueNAS SCALE: **System → Advanced → Reporting → Graphite**:
- Remote graphite server hostname: `192.168.88.X` (this host's IP)
- Port: `2003`
- Prefix: (leave blank or use `servers`)

The graphite-exporter listens on :2003 for collectd-style graphite pushes and
translates them using `config/graphite/graphite_mapping.yaml`.

## Useful commands

```bash
# Check service status:
sudo remora compose ps -f ~/Projects/home-monitoring/remora/compose.rem -p home-monitoring

# Stream all logs:
sudo remora compose logs -f ~/Projects/home-monitoring/remora/compose.rem -p home-monitoring --follow

# Stream one service:
sudo remora compose logs -f ~/Projects/home-monitoring/remora/compose.rem -p home-monitoring --follow mktxp

# Check a single service log:
sudo remora logs home-monitoring-snmp-exporter
sudo remora logs home-monitoring-plex-exporter
sudo remora logs home-monitoring-mktxp
sudo remora logs home-monitoring-graphite-exporter
sudo remora logs home-monitoring-prometheus
sudo remora logs home-monitoring-grafana

# Verify Prometheus scrape targets:
curl http://localhost:9090/api/v1/targets | python3 -m json.tool | grep -E '"health"|"job"'

# Test SNMP exporter against the router:
curl 'http://localhost:9116/snmp?target=192.168.88.1&module=mikrotik' | head -20

# Test mktxp metrics:
curl http://localhost:49090/metrics | head -20

# Test graphite exporter metrics:
curl http://localhost:9108/metrics | head -20
```

## Debugging

**A service exits immediately**: check its log with `remora logs <name>`. Common causes:
- Config file parse error (snmp-exporter: check `config/snmp/snmp.yml` format)
- Missing Plex token (plex-exporter will start but metrics will be empty)
- Permission denied on data volume (grafana: script chowns the volume to UID 472)
- mktxp: router not reachable on :8728 (check firewall, API enabled on MikroTik)

**mktxp cannot connect to router**: the MikroTik API must be enabled. In
RouterOS: `/ip service enable api`. Default port 8728. Check that the user
(`admin`) has API access.

**Prometheus not scraping**: check `http://localhost:9090/targets`. If a target
shows "connection refused", the exporter may have exited — check its log.

**TrueNAS not sending graphite**: ensure TrueNAS Reporting > Graphite is
configured to push to this host's IP on port 2003. The graphite-exporter will
show `graphite_exporter_received_samples_total` incrementing if data is arriving.

**Network issues between containers**: containers reach each other by IP
(172.20.0.x) or by service name via DNS (snmp-exporter, plex-exporter,
prometheus, mktxp, graphite-exporter). Prometheus uses service names in
`prometheus.yml`.

**Stale networks/state from a crashed run**: the start script sweeps orphaned
networks on the 172.20.0.0/24 subnet before starting. If you still see subnet
conflicts, run `sudo remora network ls` and remove stale entries manually.

**RUST_LOG for verbose output**:
```bash
sudo -E RUST_LOG=remora=debug ~/Projects/remora/scripts/start-monitoring.sh --foreground
```

## SNMP config format note

snmp-exporter v0.21.0 expects module names **directly at the top level** of
`snmp.yml` — no `modules:` wrapper key. Example:

```yaml
mikrotik:           # module name (matches ?module=mikrotik in Prometheus)
  walk: [...]
  auth:
    community: public
  version: 2
  metrics: [...]
```

Prometheus scrapes it as:
```
http://snmp-exporter:9116/snmp?target=192.168.88.1&module=mikrotik
```

## mktxp config note

mktxp reads `mktxp.conf` from the directory specified by `--cfg-dir`. The
`[MKTXP]` section sets global defaults (port, timeouts). Each `[RouterName]`
section defines one router. The container runs:
```
mktxp --cfg-dir /config export
```
where `/config/mktxp.conf` is bind-mounted read-only from `config/mktxp/mktxp.conf`.
