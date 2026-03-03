# Ongoing Tasks

All work is tracked in GitHub Issues. This file is a brief index.

## Open Issues

| # | Title | Kind |
|---|-------|------|
| #47 | track: runtime-tools pidfile.t kill-on-stopped bug (upstream) | upstream |
| #48 | track: runtime-tools process_rlimits broken by Go 1.19+ (upstream) | upstream |
| #49 | track: runtime-tools delete tests hardcoded for cgroupv1 (upstream) | upstream |
| #52 | epic: AppArmor / SELinux profile support | epic |
| #56 | epic: Wasm/WASI shim mode (WasmMode) | epic |
| #57 | feat(wasm): detect Wasm binary and select runtime (wasmtime/WasmEdge) | feat |
| #58 | feat(wasm): OCI Wasm artifact support | feat |
| #59 | feat(wasm): containerd-shim-wasm compatibility layer | feat |
| #60 | feat: io_uring opt-in seccomp profile | feat/low-pri |
| #61 | feat: CRIU checkpoint/restore support | feat/low-pri |
| #62 | feat: minimal --features build for embedded/IoT | feat/low-pri |
| #63 | feat(mac): AppArmor profile template (sub of #51) | feat |
| #64 | feat(mac): SELinux process label support (sub of #51) | feat |

## Conformance Baseline (as of 2026-03-03, SHA 6eb7283)

- Integration tests: **190/190 pass**
- E2E tests: **81 pass, 1 skipped**
- OCI conformance (runtime-tools): **33 PASS / 4 FAIL** (4 are unfixable upstream bugs — #47, #48, #49)
- **Published to crates.io as `pelagos v0.1.0`** (crate name; CLI binary remains `pelagos`)

## Session Notes

For historical session notes (completed work, design rationale) see git log.
