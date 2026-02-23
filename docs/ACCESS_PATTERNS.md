# Container Access Patterns

How to reach a running container from outside — from the host, another machine,
or a browser. Covers remora's current support and how the patterns map to
Kubernetes equivalents.

---

## The four patterns

### 1. Ad-hoc port forward

**What it is:** Forward a host port to a pod/container without having declared
the mapping at launch.

**Kubernetes:** `kubectl port-forward svc/grafana 3000:3000`

This exists because Kubernetes schedules pods — you don't control when or where
they start, so you can't always declare ports at launch time. `port-forward` is
a development convenience for reaching something that's already running
somewhere in the cluster.

**Remora today:** Not applicable as a use case. You control exactly when
containers start and what ports they expose, so declaring `(port 3000 3000)` in
`compose.rem` is already the right answer. The scenario that motivates
`kubectl port-forward` — needing to reach a container whose ports weren't
declared at launch — doesn't arise in remora.

---

### 2. NodePort — static host port

**What it is:** Bind a specific port on the host at container start time. The
service is reachable at `<host-ip>:<port>` from anywhere that can reach the
host.

**Kubernetes:** `Service` of type `NodePort`. Kubernetes picks a port in the
`30000–32767` range (or you declare one), and every node in the cluster binds
it.

**Remora today:** Fully supported. `(port 9090 9090)` in `compose.rem` or
`--publish 9090:9090` on `remora run` binds port 9090 on the host and forwards
it to port 9090 in the container via nftables DNAT + a userspace TCP proxy for
localhost access. Works on both `localhost` and the machine's LAN IP.

**Example** (from the home monitoring stack):
```lisp
(service prometheus
  (port 9090 9090)
  ...)
```

Access from host: `http://localhost:9090` or `http://192.168.88.x:9090`

---

### 3. LoadBalancer — standard port on node IP

**What it is:** Expose the service on a standard port (80, 443, etc.) directly
on a stable IP. In cloud environments this provisions a real external load
balancer. Locally it binds to the node's IP on the declared port.

**Kubernetes:** `Service` of type `LoadBalancer`. On k3s: ServiceLB
(klipper-lb) handles this automatically. On minikube: `minikube tunnel` is
required.

**Remora today:** Functionally identical to NodePort on a single host. There is
no distinction between a "load balancer IP" and "localhost" — the host *is* the
node. `(port 80 80)` achieves the same result. No gap for single-host use.

---

### 4. Ingress — reverse proxy routing by hostname or path

**What it is:** A single external port (typically 80/443) fronted by a reverse
proxy that routes requests to different backend services based on the `Host:`
header or URL path. Enables clean URLs (`http://grafana.local`) without
allocating a separate host port per service.

**Kubernetes:** An ingress controller (Traefik, nginx, Caddy) watches `Ingress`
resources and routes accordingly. k3s ships Traefik by default. minikube
provides nginx-ingress as an addon.

**Remora today:** No built-in ingress controller. However, all the infrastructure
needed to run one is present: named networks, DNS service discovery, and port
mapping. Add Traefik or Caddy as a service in `compose.rem` — it can reach all
other services by name via remora's DNS daemon.

**Example** — adding Caddy as an ingress to the monitoring stack:
```lisp
(service caddy
  (image "caddy:latest")
  (network monitoring)
  (port 80 80)
  (bind-mount "./config/caddy/Caddyfile" "/etc/caddy/Caddyfile" :ro))
```

```
# Caddyfile
grafana.local {
    reverse_proxy grafana:3000
}
prometheus.local {
    reverse_proxy prometheus:9090
}
```

Add to `/etc/hosts`:
```
127.0.0.1  grafana.local prometheus.local alertmanager.local
```

Then `http://grafana.local` routes through Caddy to the grafana container by
service name. No remora changes required — just configuration.

---

## Summary

| Pattern | Kubernetes | Remora | Status |
|---------|-----------|--------|--------|
| Ad-hoc port forward | `kubectl port-forward` | Not applicable — ports declared at launch | N/A |
| Static host port | `NodePort` | `(port X X)` in compose | ✅ Full support |
| Standard port on node IP | `LoadBalancer` | Same as NodePort on single host | ✅ No gap |
| Hostname/path routing | `Ingress` + controller | Add Traefik/Caddy as compose service | ✅ Composable, not built-in |

---

## Current monitoring stack

The home monitoring stack uses the NodePort pattern exclusively — every service
has a dedicated host port:

| Service | Host port | URL |
|---------|-----------|-----|
| Grafana | 3000 | http://localhost:3000 |
| Prometheus | 9090 | http://localhost:9090 |
| Alertmanager | 9093 | http://localhost:9093 |
| mktxp | 49090 | http://localhost:49090/metrics |
| snmp-exporter | 9116 | http://localhost:9116/metrics |
| graphite-exporter | 9108 | http://localhost:9108/metrics |
| truenas-api-exporter | 9109 | http://localhost:9109/metrics |
| plex-exporter | 9594 | http://localhost:9594/metrics |

Container-to-container traffic (e.g. Prometheus scraping exporters) uses
service names (`mktxp`, `snmp-exporter`, etc.) which resolve via remora's
built-in DNS daemon on the `monitoring` bridge network. These names are not
reachable from the host.
