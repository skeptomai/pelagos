# Pelagos Cold-Start Benchmark Results

Measurements from `scripts/bench-coldstart.sh` — `pelagos run --rm alpine /bin/true`.

For comparison: crun ~153 ms, youki ~198 ms, runc ~352 ms (standard OCI bundle, different hardware).
Pelagos's numbers reflect cached image layers; the dominant cost is namespace + cgroup setup.

---

## 2026-03-03T00:17:55Z

- **Kernel:** 6.18.13-arch1-1
- **Binary:** pelagos 0.1.0
- **Runs:** 20 (warmup: 3)
- **Command:** `pelagos run --rm alpine /bin/true`
- **Result:** 3.3 ms  (mean 3.3 ms, stddev 0.6 ms, min 2.5 ms, max 4.3 ms)

| Command | Mean [ms] | Min [ms] | Max [ms] | Relative |
|:---|---:|---:|---:|---:|
| `/home/cb/Projects/pelagos/scripts/../target/release/pelagos run --rm alpine /bin/true` | 3.3 ± 0.6 | 2.5 | 4.3 | 1.00 |
