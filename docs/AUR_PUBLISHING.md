# Publishing to the AUR

Pelagos ships two AUR packages:

| Package | Description |
|---|---|
| `pelagos` | Source build — compiles from tarball with `cargo` |
| `pelagos-bin` | Pre-built binary — downloads arch-specific release artifact |

Package definitions live in `pkg/aur/` in this repo.

## One-time setup

1. Create an account at https://aur.archlinux.org/register (username + email + SSH public key).
2. Add your SSH public key under *My Account → SSH Public Key*.

## Initial publication (claim the name)

Run once per package from inside its directory:

```bash
cd pkg/aur/pelagos
git init
git remote add aur ssh://aur@aur.archlinux.org/pelagos.git
git add PKGBUILD .SRCINFO pelagos.install
git push aur master
```

```bash
cd pkg/aur/pelagos-bin
git init
git remote add aur ssh://aur@aur.archlinux.org/pelagos-bin.git
git add PKGBUILD .SRCINFO pelagos-bin.install
git push aur master
```

Pushing to a package name that does not yet exist **creates it** and makes you the maintainer. There is no approval gate.

## Updating at release time

**Wait for the release CI workflow to complete successfully before updating the AUR.**
The sha256sums are derived from the final release artifacts, which are only produced
after all CI gates pass.

1. Bump `pkgver` (and reset `pkgrel=1`) in both `PKGBUILD` files and `Cargo.toml`.
2. Push the version bump commit and tag — CI builds the release and uploads artifacts.
3. Once CI is green, fetch the real sha256sums from the release:
   ```bash
   curl -sL https://github.com/pelagos-containers/pelagos/releases/download/v<VERSION>/pelagos-x86_64-linux.sha256
   curl -sL https://github.com/pelagos-containers/pelagos/releases/download/v<VERSION>/pelagos-aarch64-linux.sha256
   curl -sL https://github.com/pelagos-containers/pelagos/archive/refs/tags/v<VERSION>.tar.gz | sha256sum
   ```
4. Update `sha256sums` in both `PKGBUILD` files.
5. Regenerate `.SRCINFO` in each directory:
   ```bash
   makepkg --printsrcinfo > .SRCINFO
   ```
6. Commit the changes to this repo, then push to each AUR remote:
   ```bash
   git push aur master
   ```

## Testing the AUR package locally

If you have a manually-installed build in `/usr/local/bin`, remove it first so it doesn't shadow the AUR-installed binary:

```bash
sudo rm \
  /usr/local/bin/pelagos \
  /usr/local/bin/pelagos-dns \
  /usr/local/bin/pelagos-shim-wasm
```

Then install and verify:

```bash
yay -S pelagos-bin
which pelagos        # should be /usr/bin/pelagos
pacman -Q pelagos-bin
pelagos run --rm alpine echo hello
```

## Rules for AUR repos

- The AUR git repo must contain **only** `PKGBUILD`, `.SRCINFO`, and any `.install` or patch files — not the upstream source.
- `.SRCINFO` must stay in sync with `PKGBUILD`; the AUR web UI derives its metadata from `.SRCINFO`.
- Each package is a separate AUR git repo with its own remote.

## Maintainership

Co-maintainers can be added via the AUR web UI (*Package Actions → Manage Co-Maintainers*). To transfer or orphan a package, use *Package Actions → Disown*.
