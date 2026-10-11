# Releasing Conduit

Maintainer guide: pick a version, bump it, merge, push a tag. The tag push runs the whole
publishing pipeline.

## 1. Choose the version

Conduit follows [SemVer](https://semver.org/): `patch` for fixes, `minor` for backward-compatible
features, `major` for breaking changes (config or CLI incompatibilities).

## 2. Bump it

On a branch off `main`:

```bash
python3 scripts/bump-version.py minor --dry-run   # major | minor | patch | X.Y.Z[-pre]: prints the plan
python3 scripts/bump-version.py minor             # edits files and refreshes Cargo.lock
```

The script keeps these in lockstep: the workspace version and every `lopatnov-conduit-*`
dependency version in the root `Cargo.toml`, `Cargo.lock`, `npm/package.json`, and the version
strings in `docs/benchmarks.md`, `docs/cli.md` and `docs/deployment.md`. It finishes by running
`scripts/check-workspace-versions.sh`. It does not edit the changelog, commit or tag.

Then, by hand:

1. In `CHANGELOG.md`, rename `[Unreleased]` to the new version and date, and open a fresh
   `[Unreleased]` above it.
2. Open a PR, wait for CI to be green, and merge it into `main`.

## 3. Publish

Tags can only be pushed by a maintainer with write access:

```bash
git checkout main && git pull
git tag v2.1.0          # the version you just merged
git push origin v2.1.0
```

Pushing a `v*` tag starts `.github/workflows/release.yml`, which:

| Step | Result |
| --- | --- |
| Build | Binaries for every supported platform and feature bundle |
| Attest | Build-provenance attestations for the binaries |
| GitHub Release | Release notes, binaries and `SHA256SUMS.txt` |
| crates.io | All `lopatnov-conduit-*` crates, in dependency order |
| GHCR | Docker images (standard and `-full`), scanned with Trivy |
| npm | `@lopatnov/conduit`, with a `checksums.json` for the postinstall download |

Required repository secrets: `CARGO_REGISTRY_TOKEN` (crates.io) and `NPM_TOKEN` (npm). The image
push uses the workflow's own token.

Never move or re-push a tag that has already started a release. If it is wrong, publish a new
patch version instead.

## 4. crates.io rate limits

crates.io limits new crates to a burst of about 5, then one per 10 minutes; new versions of
existing crates get a burst of 30, then one per minute. The publish step waits out these limits
and skips crates that are already published, so a long run is normal for a version that adds
crates, and a re-run is safe.

If the job still stops (timeout, outage), run the **Publish crates (resume)** workflow from the
Actions tab with the tag as input (`v2.1.0`). It publishes only what is missing.

## 5. Verify

```bash
gh release view v2.1.0                                   # notes and binaries present
npm view @lopatnov/conduit version                       # matches the tag
docker buildx imagetools inspect ghcr.io/lopatnov/conduit:2.1.0
curl -s https://crates.io/api/v1/crates/lopatnov-conduit-core | jq .crate.newest_version
```

Optionally add a short summary above the generated release notes with the **Release summary**
workflow (inputs: tag and summary text).
