# Development agreement

- Read `PLAN.md` and keep `README.md` honest about implemented capabilities.
- Work on a dedicated `feat/*` or `fix/*` branch, open a PR, require green CI
  and human review; do not land changes by pushing directly to `main`.
- Keep calibration policy and session validation in `calibration-core`.
  It must not depend on a UI toolkit, audio device API or network transport.
- Keep file/device/network I/O and scheduling outside the pure core.
- Treat observations as untrusted: validate finite values, identity, timing,
  coverage and quality before making or applying a proposal.
- Never silently turn simulated observations into measured evidence.
- Preserve the original session and absolute output targets. Applying a profile
  twice must not add corrections twice. Reject stale installation snapshots.
- Keep raw recordings and machine-specific device/media paths out of Git.
- Future audio callbacks must remain bounded and allocation-free; prepare
  signals/filters and run analysis outside the realtime path.
- Run `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets --locked -- -D warnings`
  and `cargo test --workspace --locked` before requesting review.
