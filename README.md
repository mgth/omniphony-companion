# Omniphony Companion

Acoustic calibration companion for [Omniphony](https://github.com/mgth/Omniphony).

**Status: offline calibration core and live renderer measurement control.**
The companion processes already-estimated observations and proposes gains/delays.
It can also discover Omniphony outputs and play finite, acknowledged sweeps through
a temporary measurement session. Microphone capture, acoustic analysis and applying
calibration profiles to the renderer remain to be implemented.
The offline demonstration is explicitly simulated.

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

## Renderer measurement control

Requires an Omniphony build with OSC contract revision 9 and measurement protocol
version 1. The adapter uses its existing local TCP listener, normally port 9000.
It shares the typed protocol through a pinned revision of Omniphony's
`omniphony-osc-contract` crate; no local sibling checkout is needed to build.

```sh
cargo run --locked -p companion-cli -- renderer discover --out renderer-snapshot.json
cargo run --locked -p companion-cli -- renderer sweep --snapshot renderer-snapshot.json --output L --duration-ms 1000 --peak-dbfs -30 --start-hz 200 --end-hz 10000 --out-dir sessions/first-sweep
```

Use an output ID returned by discovery; `L` is an example. Pass `--endpoint
127.0.0.1:9003` to either command when using a renderer on another port.
The sweep command **plays sound**. It creates a new directory containing the
initial snapshot, stimulus request and completed render report. It records no
microphone data and makes no acoustic accuracy claim. Existing directories are
refused before playback.

The renderer owns stop deadlines, enforces one session owner, rejects stale
configurations and blocks other control writes until the session ends. During
the session it silences programme audio and emits the sweep only on the selected
output, through existing output gain and delay. The sweep bypasses spatial
panning/crossovers, stays within the output's declared frequency band and has
250 ms settling silence before and after. No settings are saved or changed.

The CLI polls every 200 ms to renew the three-second lease. Ctrl+C disconnects
it; the renderer stops the abandoned session. The Rust adapter also exposes
explicit `stop` and `end` operations. A host that does not render expires the
signal instead of reporting completion. Render progress is not a DAC timestamp
or an acoustic timing reference.

See the [wire protocol](https://github.com/mgth/Omniphony/blob/08f0bc00dae820981762278748e7a620d54ca6ae/docs/measurement-protocol.md)
for limits, identifiers and the hardware-free `measurement_fixture` bench.

## Offline calibration capabilities

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
- `crates/omniphony-adapter`: typed TCP requests, acknowledgements, session
  ownership and protocol validation; independent of the calibration core and UI.

The core accepts arrival/level observations; estimating direct arrivals from
recordings remains to be implemented. Application functions return a target
state: a future adapter must verify and apply it atomically in the renderer.
Profile application and restoration have no live network implementation yet.
The adapter's snapshots and render reports are separate from acoustic observation
sessions; renderer instance IDs are not permanent installation IDs.

The next increment is single-microphone capture with a shared timing reference,
clock handling and retained raw recordings. Acoustic analysis, the graphical
assistant, automatic iterations and advanced corrections follow after that.

## Development checks

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
cargo +1.89.0 check --workspace --all-targets --locked
```

CI is configured to run these checks, the demonstration and a Rust 1.89 build.
Changes go through a dedicated branch and a PR for review.

All repository content and project communication must be in English. See
[the development agreement](AGENTS.md).
