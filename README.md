# Omniphony Companion

Acoustic calibration companion for [Omniphony](https://github.com/mgth/Omniphony).

**Status: first offline increment of the delay and level mission.**
The prototype processes already-estimated observations and proposes gains/delays.
It does not yet capture microphones, analyze PCM or control a real renderer.
The demonstration is explicitly simulated.

The [development plan](PLAN.md) describes the first mission's seven milestones
and later extensions: bass, subwoofers, reflections and overall refinement.

## Try it

Requires Rust 1.89 or later. No audio device or Omniphony service is needed.

```sh
cargo run --locked -p companion-cli -- demo --out-dir sessions/demo
cargo run --locked -p companion-cli -- inspect --session sessions/demo/session.json
cargo run --locked -p companion-cli -- plan --session sessions/demo/session.json --out sessions/proposed.json
cargo run --locked -p companion-cli -- verify --session sessions/demo/session.json --profile sessions/proposed.json --measurements sessions/demo/verification-measurements.json --out sessions/checked.json
```

The demonstration represents three speakers, a subwoofer preserved without
automatic correction, two microphones and two positions. The initial policy
aligns speakers only at the reference position. A second set of observations
is generated from the modified rig state, then verified. The rig is subsequently
restored to its initial settings.

At the reference position, the scenario reduces arrival spread from 2 ms to
0 ms and level spread from 5 dB to 0 dB. These are deterministic simulator
results, not measured accuracy in a real acoustic environment.

The command produces:

- `session.json`: installation, microphones, positions and initial observations;
- `profile.json`: policy, initial state and proposed absolute settings;
- `verification-measurements.json`: a new set of observations;
- `verification-report.json`: before/after deviations and verification status.

Existing output paths are refused. Choose a new directory to run the demonstration
again. Local sessions are ignored by Git.

## First-increment capabilities

- Versioned JSON model with output, microphone and position identifiers.
- Validation of values, references, duplicates and reference-position coverage.
- Rejection of clipped, unsynchronized or insufficient-SNR measurements.
- Conservative proposals: delay early arrivals and attenuate louder speakers,
  accounting for already-active gains/delays.
- Separate verification using observations after application.
- Idempotent application/restoration preparation that rejects stale state or
  edits made since measurement.
- Document persistence without overwriting existing files.

Imported observations must already be comparable: the same stimulus/emission
level, level measurement method, microphone calibration and timing reference.
Declarative fields alone do not establish this quality. The future acquisition
service must measure and record it.

The initial policy requires the same microphone for observations at the reference
position. Other positions and microphones are retained for later stages; they
are not averaged and do not contribute to the current verdict.

`verify` returns 0 when tolerances are met, 2 when valid measurements fail the
tolerances, and 1 when data or files are invalid. Default tolerances (0.1 ms /
0.5 dB) are prototype thresholds requiring hardware validation. The report
does not assess the entire listening area.

## Architecture and limitations

- `crates/calibration-core`: models, validation, proposals and verification;
  no audio hardware, network, file access, timers or UI toolkit.
- `crates/companion-cli`: commands, files and the simulated rig.

The core accepts arrival/level observations; estimating direct arrivals from
recordings remains to be implemented. Application functions return a target
state: a future adapter must verify and apply it atomically in the renderer.
There is no network transaction or automatic recovery from hardware failure yet.

The next increment is a measurement-session contract with Omniphony, followed
by microphone capture and timing references. Real capture, sweeps, clock
compensation, the graphical assistant, automatic iterations and advanced
acoustic corrections remain to be implemented.

## Development checks

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
```

CI is configured to run these checks, the demonstration and a Rust 1.89 build.
Changes go through a dedicated branch and a PR for review.

All repository content and project communication must be in English. See
[the development agreement](AGENTS.md).
