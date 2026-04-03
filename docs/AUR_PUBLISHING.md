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

1. Bump `pkgver` (and reset `pkgrel=1`) in both `PKGBUILD` files.
2. Replace `sha256sums=('SKIP')` with real hashes from the release artifacts:
   ```bash
   sha256sum pelagos-x86_64-linux
   sha256sum pelagos-aarch64-linux
   sha256sum v<VERSION>.tar.gz
   ```
3. Regenerate `.SRCINFO` in each directory:
   ```bash
   makepkg --printsrcinfo > .SRCINFO
   ```
4. Commit the changes to this repo, then push to each AUR remote:
   ```bash
   git push aur master
   ```

## Rules for AUR repos

- The AUR git repo must contain **only** `PKGBUILD`, `.SRCINFO`, and any `.install` or patch files — not the upstream source.
- `.SRCINFO` must stay in sync with `PKGBUILD`; the AUR web UI derives its metadata from `.SRCINFO`.
- Each package is a separate AUR git repo with its own remote.

## Maintainership

Co-maintainers can be added via the AUR web UI (*Package Actions → Manage Co-Maintainers*). To transfer or orphan a package, use *Package Actions → Disown*.
