# Products — Deploy Profiles

Each product declares a `deploy_profile` that tells the ceremony flow which deploy
stages to run. Products also declare `deploy_app_id` when they use the Connect App
Pipeline for deployment.

## Deploy Profiles

| Profile | Behavior | Example |
|---------|----------|---------|
| `none` | Skip entire deploy chain (deploy_standby, gate_deploy_ok, judge_ab, gate_ab, promote) | CLI tools, libraries |
| `connect_app` | Blue-green deploy via Connect App Pipeline + A/B judge verification | Frontend apps (console, admin) |
| `bootstrap` | Rust binary deploy via Bootstrap Pipeline | Rust API services |

## Product Registry

### epic-runner

| Field | Value |
|-------|-------|
| **Slug** | `epic-runner` |
| **Prefix** | `ER` |
| **Lang** | Rust |
| **deploy_profile** | `none` |
| **deploy_app_id** | — |

Epic Runner is a CLI tool. It doesn't deploy via Connect App Pipeline or Bootstrap.
The ceremony flow skips the deploy chain entirely for this product.

### console

| Field | Value |
|-------|-------|
| **Slug** | `console` |
| **Prefix** | `S` |
| **Lang** | TypeScript |
| **Framework** | React Router v7 + Bun BFF |
| **deploy_profile** | `connect_app` |
| **deploy_app_id** | `9ee900e7-3d10-46f1-b59b-bade220cfaa4` |

Kapable Org Console — deployed via Connect App Pipeline with blue-green slot routing.

### admin

| Field | Value |
|-------|-------|
| **Slug** | `admin` |
| **Prefix** | `A` |
| **Lang** | TypeScript |
| **deploy_profile** | `connect_app` |
| **deploy_app_id** | `abee3d58-259b-4454-9147-df67c0b74de6` |

### developer

| Field | Value |
|-------|-------|
| **Slug** | `developer` |
| **Prefix** | `D` |
| **Lang** | TypeScript |
| **deploy_profile** | `connect_app` |
| **deploy_app_id** | `81e66cfd-84fa-4cae-a497-2d7f07e8f801` |

### kapable-api

| Field | Value |
|-------|-------|
| **Slug** | `kapable-api` |
| **Prefix** | `API` |
| **Lang** | Rust |
| **deploy_profile** | `bootstrap` |
| **deploy_app_id** | — |

Deployed via the Bootstrap Pipeline (cross-compiled Rust binary + migrations).

---

## Definition of Done by Deploy Profile

### connect_app DoD (Frontend Apps)

Products with `deploy_profile: connect_app` must satisfy these additional DoD gates:

| Check | Required | Description |
|-------|----------|-------------|
| Blue-green health | Yes | Standby container responds 200 on `/health` via `X-Slot: standby` header |
| Session continuity | Yes | Authenticated session cookies work on standby (same domain, same DB) |
| A/B judge approval | Yes | A/B judge verifies standby vs live and approves promotion |
| Promote success | Yes | Standby promoted to primary without downtime |

**Infrastructure gate**: Blue-green health is an infrastructure prerequisite. If the standby container is unhealthy, the deploy chain halts at `gate_deploy_ok`. The code judge flags this as an infrastructure issue — it does NOT affect the code quality verdict.

### bootstrap DoD (Rust Services)

Products with `deploy_profile: bootstrap` must satisfy:

| Check | Required | Description |
|-------|----------|-------------|
| Health endpoint | Yes | `/health` returns 200 after deploy |
| Metrics endpoint | Yes | `/metrics` returns valid Prometheus format |

### none DoD (CLI Tools, Libraries)

Products with `deploy_profile: none` have no deploy-related DoD gates. Evaluation is purely on code quality and test results.
