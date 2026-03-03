# Multi-Stage Build Example

Demonstrates multi-stage builds, `ARG`, `.remignore`, and `COPY --from` with Pelagos.

## What This Shows

- **Multi-stage build**: Stage 1 (`builder`) installs the full Rust toolchain and compiles
  the binary. Stage 2 copies only the release binary into a clean Alpine image.
- **ARG**: `PROFILE=release` is the default; override with `--build-arg PROFILE=debug`.
- **`.remignore`**: Excludes `target/`, `.git/`, and `*.md` from the build context.
- **`COPY --from=builder`**: Copies the compiled binary from the builder stage.

## Build and Run

```bash
# Pull the base image
sudo pelagos image pull alpine

# Build the image
sudo pelagos build -t hello-server:latest examples/multi-stage/

# Run with port mapping
sudo pelagos run --name hello -p 8080:8080 hello-server:latest

# Test it
curl http://localhost:8080
# => {"hostname":"...","version":"0.1.0","timestamp":...}

# Clean up
sudo pelagos stop hello
sudo pelagos rm hello
```

## Debug Build

```bash
sudo pelagos build -t hello-server:debug --build-arg PROFILE=debug examples/multi-stage/
```

## Image Size

The final image contains only Alpine (~5 MB) plus the statically-linked binary (~2 MB),
rather than the full Rust toolchain (~500 MB+). This is the key benefit of multi-stage builds.
