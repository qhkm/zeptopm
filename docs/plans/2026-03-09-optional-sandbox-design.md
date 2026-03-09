# Optional Sandbox (ZeptoKernel) Design

**Goal:** Make zeptokernel an optional dependency in zeptoPM so it works without isolation support, with clean DX and agent experience.

**Approach:** Cargo feature flag `capsule` (default-enabled) + CLI `--sandbox`/`--no-sandbox` flags that override TOML config.

## Feature Flag

```toml
# Cargo.toml
[features]
default = ["capsule"]
capsule = ["dep:zeptokernel"]
```

- `cargo build` — includes capsule support (same as today)
- `cargo build --no-default-features` — no zeptokernel dependency

## CLI Flags

```bash
zeptopm daemon                     # uses TOML default
zeptopm daemon --sandbox           # force capsule isolation
zeptopm daemon --no-sandbox        # force plain process spawning
```

`--sandbox` and `--no-sandbox` are mutually exclusive (clap `conflicts_with`).

## Resolution Order

```
CLI --sandbox/--no-sandbox  >  TOML isolation field  >  default "none"
```

## Graceful Degradation

When capsule isolation is requested but unavailable:

1. **Feature compiled out** (`--no-default-features`):
   - Startup warning: `--sandbox requires zeptoPM built with 'capsule' feature — falling back to direct process spawning`
   - Jobs run without sandbox via plain agent spawning

2. **Feature compiled in but host doesn't support it** (e.g., namespace on macOS):
   - ZeptoKernel returns `NotSupported`
   - Job event: `job_failed` with `retryable: true`
   - (Existing behavior, unchanged)

## Module Gating

```rust
// lib.rs
#[cfg(feature = "capsule")]
pub mod capsule;

// daemon.rs
#[cfg(feature = "capsule")]
if sandbox_active { /* capsule path */ }
// always falls through to plain agent spawning
```

## Integration Tests

`tests/capsule_integration.rs` gated with `#![cfg(feature = "capsule")]`.

## CI

```yaml
- cargo test                          # with capsule (default)
- cargo test --no-default-features    # without capsule
```

## What Does NOT Change

- Default build includes everything (backwards compatible)
- TOML `isolation` field still works
- All existing tests pass
- capsule.rs bugfix (firecracker/fallback fields) included
