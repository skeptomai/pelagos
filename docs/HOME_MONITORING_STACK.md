# Home Monitoring Stack

The home monitoring stack lives in the
[home-monitoring](https://github.com/skeptomai/home-monitoring) repository
under `remora/`. It is a real production deployment of `remora compose`
monitoring home infrastructure: MikroTik router, TrueNAS NAS, and Plex
Media Server.

## Stack

| Service | Image | Purpose |
|---------|-------|---------|
| snmp-exporter | prom/snmp-exporter:v0.21.0 | MikroTik router SNMP walk |
| plex-exporter | ghcr.io/axsuul/plex-media-server-exporter | Plex REST API |
| mktxp | ghcr.io/akpw/mktxp | MikroTik RouterOS API |
| graphite-exporter | prom/graphite-exporter | TrueNAS collectd push receiver |
| truenas-api-exporter | truenas-api-exporter (local build) | TrueNAS SCALE REST API |
| alertmanager | prom/alertmanager | Alert routing |
| prometheus | prom/prometheus | Scrape + store + query |
| grafana | grafana/grafana | Dashboards |

## Running

```bash
cd ~/Projects/home-monitoring/remora
sudo ./start.sh              # start (background)
sudo ./start.sh --foreground # start with live logs
./check.sh                   # endpoint health check
sudo ./start.sh --down       # stop
```

## Relation to examples/compose/monitoring/

`examples/compose/monitoring/` is a self-contained demo stack (Alpine +
APK-built Prometheus/Loki/Grafana) designed to showcase remora's compose
features. It has no connection to this production stack.

This stack uses upstream production images, real device targets, and reads
secrets from `.env`. Both use the `.reml` Lisp compose format.
