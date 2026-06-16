# Dracon System

Disk, zram, process, service, guard — local machine diagnostics and watchdog for Dracon workspaces.

This repository is a feature façade for `dracon-system`. It does **not**
duplicate the implementation code. The canonical source of truth remains the
[`DraconDev/dracon-utilities`](https://github.com/DraconDev/dracon-utilities)
monorepo, with this utility's code and docs under:

- Source: [`dracon-system/`](https://github.com/DraconDev/dracon-utilities/tree/main/dracon-system)
- User guide: [`dracon-system/README.md`](https://github.com/DraconDev/dracon-utilities/tree/main/dracon-system/README.md)
- Design notes: [`dracon-system/BLUEPRINT.md`](https://github.com/DraconDev/dracon-utilities/tree/main/dracon-system/BLUEPRINT.md)
- Example config: [`dracon-system/dracon-system.example.toml`](https://github.com/DraconDev/dracon-utilities/tree/main/dracon-system/dracon-system.example.toml)

## Why this name?

The descriptive name is a deliberate choice for Codeberg/Forgejo, where
descriptive repo names get upvotes and free attention because readers
immediately know what the project does. The full word list (no fillers, no
audience/UX claims) is documented in
[`docs/design/github-feature-repos.md`](https://github.com/DraconDev/dracon-utilities/blob/main/docs/design/github-feature-repos.md).

## Purpose

Protects machines from disk/process pressure and provides deterministic diagnostics for storage, links, zram, events, and the guard daemon.

Use this repo to feature the utility on GitHub, GitLab, and Codeberg without
splitting the actual implementation out of the monorepo. Issues, project
boards, and roadmap notes can live here, while commits, releases, tests, and
packaging stay anchored in `dracon-utilities`.

## Runtime

- Binary: `dracon-system`
- Service: dracon-system-guard.service
- Example policy: `dracon-system/dracon-system.example.toml`
- Common commands: `dracon-system status · dracon-system doctor · dracon-system storage · dracon-system guard daemon`

## Relationship to the monorepo

| Boundary | Decision |
|----------|----------|
| Source code | Lives in `dracon-utilities/dracon-system` |
| Release artifacts | Built and published from `dracon-utilities` |
| Feature surface | This façade repo (and short-name alias) |
| Operational policy | `~/.dracon/utilities/` TOML files |
| Shared libraries | Sibling `dracon-libs` workspace where applicable |

## Maintenance

When the monorepo changes the utility README, blueprint, or example config,
regenerate this façade with:

```bash
cd /path/to/dracon-utilities
./scripts/scaffold_feature_repos.py --apply --repo dracon-system
./scripts/scaffold_feature_repos.py --push-all-remotes --repo dracon-system \
    --ssh-target /path/to/dracon-system-disk-zram-process-service-guard
```

Do not paste implementation code into this façade repo. Keep it as a stable
navigation and feature surface so the monorepo remains the single source of
truth.

## License

AGPL-3.0-only — see [LICENSE](LICENSE).
