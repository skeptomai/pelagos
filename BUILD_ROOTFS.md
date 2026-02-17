# Building Alpine Linux Rootfs

Remora requires an Alpine Linux root filesystem to run containers. You have two options:

## Option 1: Using Docker (Recommended)

**Script:** `./build-rootfs-docker.sh`

**Requires:**
- Docker installed and running
- sudo privileges

**Advantages:**
- Always gets the latest Alpine version
- Official Docker image (well-tested)
- Simple and reliable

**Usage:**
```bash
./build-rootfs-docker.sh
```

**What it does:**
1. Cleans up any existing rootfs
2. Pulls `alpine:latest` from Docker Hub
3. Extracts the filesystem from the image
4. Verifies the build

## Option 2: Download Tarball (No Docker)

**Script:** `./build-rootfs-tarball.sh`

**Requires:**
- curl or wget
- tar
- sudo privileges

**Advantages:**
- No Docker daemon required
- Works on minimal systems
- Direct from Alpine Linux CDN

**Usage:**
```bash
./build-rootfs-tarball.sh
```

**What it does:**
1. Detects your architecture (x86_64 or aarch64)
2. Downloads Alpine minirootfs tarball from Alpine CDN
3. Extracts with proper permissions
4. Sets ownership to current user
5. Verifies the build

**Note:** The tarball script specifies a version (currently 3.21.0). Edit the script to change versions:
```bash
ALPINE_VERSION="3.21"   # Major.Minor version
ALPINE_MINOR="3.21.0"   # Full version with patch
```

## Verifying the Build

Both scripts verify the build and show:
- Architecture (should match your system)
- Essential tools (busybox, ash)
- Total size (should be ~5-10 MB)

**Manual verification:**
```bash
ls -lh alpine-rootfs/bin/busybox
file alpine-rootfs/bin/busybox
```

## Testing

After building, test with:

```bash
# Build remora first
cargo build

# Run the CLI
sudo -E ./target/debug/remora \
  --exe /bin/ash \
  --rootfs ./alpine-rootfs \
  --uid 1000 \
  --gid 1000

# Or run the seccomp demo
sudo -E cargo run --example seccomp_demo

# Or run integration tests
sudo -E cargo test --test integration_tests
```

## Troubleshooting

### Docker script fails: "Docker daemon is not running"
Start Docker first:
```bash
sudo systemctl start docker  # systemd
# or
sudo service docker start    # sysvinit
```

Or use the tarball script instead: `./build-rootfs-tarball.sh`

### Tarball script fails: "Unsupported architecture"
The tarball script only supports x86_64 and aarch64. Use the Docker script instead.

### Permission errors during extraction
Both scripts use `sudo` for extraction to preserve file ownership and permissions. This is normal and required.

### "alpine-rootfs not found" when running remora
Make sure you're running from the project root directory where `alpine-rootfs/` was created.

## Cleaning Up

To remove the rootfs and start fresh:

```bash
sudo umount alpine-rootfs/sys 2>/dev/null || true
sudo umount alpine-rootfs/proc 2>/dev/null || true
sudo umount alpine-rootfs/dev 2>/dev/null || true
sudo rm -rf alpine-rootfs
```

Then run either build script again.

## Which Script Should I Use?

**Use Docker script if:**
- ✅ You have Docker installed
- ✅ You want the latest Alpine automatically
- ✅ You trust Docker Hub

**Use Tarball script if:**
- ✅ You don't have Docker
- ✅ You're on a minimal system
- ✅ You want to pin a specific Alpine version
- ✅ You prefer downloading directly from Alpine

Both produce equivalent rootfs environments suitable for remora containers.
