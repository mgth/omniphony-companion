# Omniphony Companion

Acoustic calibration companion for [Omniphony](https://github.com/mgth/Omniphony).

**Status: single-microphone capture, timing diagnostics and an offline calibration core.**
The companion discovers renderer outputs, plays finite measurement sequences,
records a selected interface input channel and saves raw audio with quality reports.
The offline core can propose gains/delays from already-estimated observations.
Direct-arrival analysis and live application of calibration profiles are still
pending. The demonstration is explicitly simulated.

The [development plan](PLAN.md) describes the first mission's seven milestones
and later extensions: bass, subwoofers, reflections and overall refinement.

## Try it

Requires Rust 1.89 or later. On Linux, install ALSA development headers and
`pkg-config` (`sudo apt-get install libasound2-dev pkg-config`). The offline
demonstration needs no audio device or Omniphony service.

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
version 2 from the calibration development PR. The adapter uses its existing local TCP listener, normally port 9000.
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
refused before playback. Add `--reference-output L` to exercise the three-segment
sequence without microphone capture.

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

See the [wire protocol](https://github.com/mgth/Omniphony/blob/defdec01c507b38c600bae08e29aca3bf1987d38/docs/measurement-protocol.md)
for limits, identifiers and the hardware-free `measurement_fixture` bench.

## Single-microphone acquisition

The first workflow targets an audio interface with one measurement microphone.
CPAL uses the platform default host (ALSA on Linux, CoreAudio on macOS, WASAPI
on Windows); ASIO and an electrical loopback channel are not implemented yet.
Select the exact device name and a one-based input channel:

```sh
cargo run --locked -p companion-cli -- capture devices
cargo run --locked -p companion-cli -- capture measure --snapshot renderer-snapshot.json --device "Your interface" --input-channel 1 --sample-rate 48000 --microphone-id mic-1 --position-id reference-seat --output R --reference-output L --out-dir sessions/first-capture
```

Device enumeration does not record audio. `capture measure` records the selected
channel and plays a reference–target–reference sequence. Use actual output IDs
from discovery. Set the interface's gain manually and keep it, microphone
position, routing and processing fixed across compared takes. Disable microphone
AGC/noise suppression. Absolute SPL calibration is not provided.

The renderer schedules identical swept-sine segments at known sample offsets.
The companion estimates the capture/render clock ratio from the two reference
peaks, then uses their midpoint to estimate the target's relative peak delay.
No network timestamp is used as an audio time reference. The same reference
speaker must be used throughout a comparison. Input and output may have
different nominal rates; the sweep must fit both Nyquist limits.

Each new take directory contains:

- `recording.wav`: the selected channel, float32 PCM, including lead-in and tail;
- `snapshot.json` and `stimulus.json`: settings, identities and exact stimulus;
- `capture-report.json`: input configuration, quality flags, raw level checks,
  timing diagnostics or rejection reason, and the frame origin for analysis;
- `microphone-calibration.txt`, if supplied with `--calibration`: preserved
  two-column frequency/relative-dB data. It is validated but **not applied**.

Takes are rejected on stream errors, queue overflow, timestamp gaps, clipping,
missing/ambiguous references, poor SNR or unstable reference levels. Limits
(20 dB SNR, 2000 ppm linear drift, 100 ms relative peak delay and 1 dB reference
level change) are prototype gates, not hardware accuracy guarantees.
Ctrl+C ends the session and saves the partial take as rejected. Connection
failure follows the same cleanup path. Force-killing the process or a disk
failure can prevent saving; the renderer still has its own stop limits.

**Timing reports are diagnostics, not calibration observations.** A dominant
reflection can obscure the direct sound. Two references estimate linear drift
but cannot establish clock stability throughout the take. No capture is marked
valid for correction, and there is no automatic capture-to-profile conversion.

Callbacks only convert the selected channel into a bounded queue. File writes
and FFT correlation happen off the audio thread. Captures are capped at 30 s;
the queue holds four seconds of mono samples and reports overflow instead of
hiding lost data. Offline analysis has a bounded FFT size and can use substantial
memory at high sample rates; 48 kHz is the initial bench target.

Deterministic tests cover unequal sample rates, known delays, positive/negative
clock drift, inverted polarity, noise, missing references, clipping and strong
competing reflections. Real interface/microphone repeatability remains to be
measured. See [the capture validation checklist](docs/capture-validation.md).

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
Declarative fields alone do not establish this quality. Capture diagnostics are stored separately and do not yet produce
these validated observations.

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
- `crates/companion-cli`: commands, files, acquisition scheduling and the simulated rig.
- `crates/measurement-capture`: bounded input capture, calibration-file parsing
  and offline timing diagnostics; no correction policy.
- `crates/omniphony-adapter`: typed TCP requests, acknowledgements, session
  ownership and protocol validation; independent of the calibration core and UI.

The core accepts arrival/level observations; estimating direct arrivals from
recordings remains to be implemented. Application functions return a target
state: a future adapter must verify and apply it atomically in the renderer.
Profile application and restoration have no live network implementation yet.
The adapter's snapshots and render reports are separate from acoustic observation
sessions; renderer instance IDs are not permanent installation IDs.

The next steps are real-interface validation, repeatability assessment and
impulse-response/direct-arrival analysis. The graphical assistant, automatic
iterations and advanced corrections follow after that.

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
