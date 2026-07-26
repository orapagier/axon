# Releasing Axon

How to cut a GitHub release and how end users install it.

---

## 1. Build the release artifacts (sanitized — no secrets)

> ⚠️ **Never upload `axon_deploy_cham.tar.gz` / `axon_deploy_gcp.tar.gz` or the
> `dist-cham/` `dist-gcp/` bundles.** Those are your private deploy bundles and
> contain your real `.env`, `credentials.json`, and **SSH private keys**. Use the
> packaging scripts below, which build clean bundles (`.env.example` only, no
> keys) and abort if any secret sneaks in.

**Linux bundle** (`axon-linux-x86_64.tar.gz`) — run under WSL or Git Bash from the repo root:

```bash
bash scripts/package-release.sh
```

Builds the Vue dashboard, then a **static musl** `axon` binary (portable across
distros), and assembles `axon-linux-x86_64.tar.gz`. Needs `node`/`npm` and either
[`cross`](https://github.com/cross-rs/cross) (Docker) or the
`x86_64-unknown-linux-musl` Rust target.

**Windows bundle** (`axon-windows-x86_64.zip`) — run in PowerShell from the repo root:

```powershell
powershell -ExecutionPolicy Bypass -File scripts\package-release.ps1
```

Builds the dashboard and a native `axon.exe`, then zips it with its `static\`,
`config\`, and `tools\` folders. `axon.exe` **cannot run alone** — it serves the
dashboard from `static\` and reads `config\`/`.env` from its working directory —
so the Windows release is a zip bundle, the twin of the Linux tarball.

---

## 2. Create the GitHub release

Tag the version and upload the artifacts **plus the two install scripts** so the
one-line installers can fetch them from `releases/latest/download/…`:

- `axon-linux-x86_64.tar.gz`
- `axon-windows-x86_64.zip`
- `install.sh`      (copy of `scripts/install.sh`)
- `install.ps1`     (copy of `scripts/install.ps1`)

With the GitHub CLI:

```bash
gh release create v0.4.0 \
  axon-linux-x86_64.tar.gz \
  axon-windows-x86_64.zip \
  scripts/install.sh \
  scripts/install.ps1 \
  --title "Axon v0.4.0" \
  --notes "See README for setup. Linux: install.sh · Windows: install.ps1"
```

…or draft it in the GitHub web UI and drag the four files in.

---

## 3. What users run

The installers use a local artifact if it sits in the current directory,
otherwise they download the latest release from GitHub. On first install they
**auto-generate a valid `AXON_MASTER_KEY`** (boot refuses a blank/placeholder
key) and print it — users must save it.

### Linux (Debian/Ubuntu)

Straight from GitHub:

```bash
curl -fsSL https://github.com/orapagier/axon/releases/latest/download/install.sh | bash
```

Or, having downloaded `axon-linux-x86_64.tar.gz` + `install.sh` into a folder:

```bash
bash install.sh
```

Installs a `systemd` service (`axon-agent`), optionally installs Qdrant, and
starts it. Options: `--dir PATH`, `--version vX.Y.Z`, `--file PATH`,
`--with-qdrant` / `--no-qdrant`, `--no-service`.

### Windows

In PowerShell:

```powershell
irm https://github.com/orapagier/axon/releases/latest/download/install.ps1 | iex
```

Or, having downloaded `axon-windows-x86_64.zip` + `install.ps1` into a folder:

```powershell
powershell -ExecutionPolicy Bypass -File install.ps1
```

Installs to `%LOCALAPPDATA%\Axon`, registers a hidden **Startup** launcher (runs
on login), and starts it. Options: `-Dir PATH`, `-Version vX.Y.Z`, `-File PATH`,
`-NoStartup`, `-NoStart`.

### After install (both platforms)

The dashboard is at **http://localhost:3000**. Add at least one LLM provider key
to `.env` (in `core/.env` on Linux, in the install dir on Windows), then restart
Axon. See the main [README](./README.md) and [USER_GUIDE](./USER_GUIDE.md).
