# Dracon System

Local disk, process, storage, zram, and service diagnostics for Dracon machines.

This repository is a GitHub feature façade for dracon-system. It does **not**
duplicate the implementation code. The canonical source of truth remains the
[`DraconDev/dracon-utilities`](https://github.com/DraconDev/dracon-utilities)
monorepo, with this utility's code and docs under:

- Source: [`dracon-system/`](https://github.com/DraconDev/dracon-utilities/tree/main/dracon-system)
- User guide: [`dracon-system/README.md`](https://github.com/DraconDev/dracon-utilities/tree/main/dracon-system/README.md)
- Design notes: [`dracon-system/BLUEPRINT.md`](https://github.com/DraconDev/dracon-utilities/tree/main/dracon-system/BLUEPRINT.md)
- Example config: [`dracon-system/dracon-system.example.toml`](https://github.com/DraconDev/dracon-utilities/tree/main/dracon-system/dracon-system.example.toml)

## Purpose

Protects machines from disk/process pressure and provides deterministic diagnostics for storage, links, zram, events, and the guard daemon.

Use this repo to feature the utility on GitHub without splitting the actual
implementation out of the monorepo. Issues, project boards, and roadmap notes can
live here, while commits, releases, tests, and packaging stay anchored in
`dracon-utilities`.

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
| GitHub feature surface | This façade repo |
| Operational policy | `~/.dracon/utilities/` TOML files |
| Shared libraries | Sibling `dracon-libs` workspace where applicable |

## Maintenance

When the monorepo changes the utility README, blueprint, or example config,
regenerate this façade with:

```bash
cd /path/to/dracon-utilities
./scripts/scaffold_feature_repos.py --apply --repo dracon-system
```

Do not paste implementation code into this façade repo. Keep it as a stable
navigation and feature surface so the monorepo remains the single source of
truth.

## License

AGPL-3.0-only — see [LICENSE](LICENSE).
