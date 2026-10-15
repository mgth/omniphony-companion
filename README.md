# Omniphony Companion

Acoustic calibration companion for [Omniphony](https://github.com/mgth/Omniphony).

**Status: native desktop UI, microphone capture, offline impulse-response review and an offline calibration core.**
The companion discovers renderer outputs, plays finite measurement sequences,
records a selected interface input channel and saves raw audio with quality reports.
The offline core can propose gains/delays from already-estimated observations.
Experimental arrival candidates can now be reviewed against impulse responses.
Validated direct-arrival observations and live application of calibration profiles
are still pending. The demonstration is explicitly simulated.

The [development plan](PLAN.md) describes the first mission's seven milestones
and later extensions: bass, subwoofers, reflections and overall refinement.

## Native companion UI

```sh
cargo run --locked -p companion-ui
```

The Rust/egui desktop window lets you:

- Refresh the available microphone inputs and choose an interface, input channel
  and sample rate. Selecting an input does not start recording.
- Connect to the local Omniphony address and list its physical speaker outputs,
  including current gain, delay, mute and frequency limits.
- Choose a fixed timing-reference speaker and a listening position.
- Start a measurement for one speaker, follow progress and stop it.
- Review diagnostics or rejection reasons and find the saved take folder.
- Reanalyze saved recordings without playback; view impulse envelopes and
  frequency-band arrival candidates even while disconnected from Omniphony.

**Captured · arrival candidate** means the new impulse analysis found a repeatable
early peak. **Captured · arrival needs review** means the impulse responses are
available but the timing evidence disagrees. Both are acquisition successes;
neither authorizes correction. The older correlation diagnostics remain in the saved report and may still
show **Unresolved**. Increasing volume does not resolve
arrival ambiguity.

The **Measure** button becomes available when the renderer, microphone, channel,
reference and sweep settings are usable. Each take plays a sweep on the reference,
the selected speaker, then the reference again. It uses the same acquisition
service as the CLI. A fresh renderer snapshot is checked before opening the input.
No corrections or persistent renderer settings are applied.

Connect to a build from the [Omniphony calibration PR](https://github.com/mgth/Omniphony/pull/862), normally
`127.0.0.1:9000`; the dedicated calibration workflow uses port 9003.
The UI does not launch Omniphony itself. Change the address and press **Connect**
or **Refresh speakers** after changing its speaker layout or settings.

Discovery and measurement run in background service workers. Minimizing the
window does not pause keepalives or capture. Closing it during measurement requests
cancellation and waits for cleanup so the rejected partial take can be saved.
Results are kept in the window for the current run; raw WAVs and JSON reports
remain in the recording folders. Use **Review a saved take · no playback**,
paste a take folder, and press **Analyze saved take** to reopen a recording.
A current result also has a **Reanalyze this take** action. This reads the take
and generates a fresh report; it does not restore acquisition settings.

To inspect the layout without an interface or renderer:

```sh
cargo run --locked -p companion-ui --example preview
cargo run --locked -p companion-ui --example preview -- --arrival-review
```

This fixture window is labelled **UI PREVIEW · NO AUDIO**. Its speakers and
microphone are synthetic; clicking Measure does not open hardware or create
recordings.

![Native companion layout with synthetic preview data](docs/images/companion-preview.png)

The native frontend uses egui/eframe 0.32 with OpenGL to retain the workspace's
Rust 1.89 minimum. This increment is validated on Linux; native packaging and
hardware validation on Windows/macOS remain pending. Settings are not persisted
between launches yet. See [the application boundaries](docs/ui-architecture.md).

## Try it

Requires Rust 1.89 or later. On Linux, install ALSA and PipeWire development
headers, Clang and pkg-config:
`sudo apt-get install libasound2-dev libpipewire-0.3-dev libclang-dev pkg-config`. The offline
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
Linux uses native PipeWire when its server is available. Sources have their
actual descriptions and port names, such as a USB microphone and `capture_FL`.
The channel count comes from the node's audio ports; MIDI ports are excluded.
The complete input channel map is requested without remixing, then only the
selected channel is retained. The target is pinned for the take; removal rejects
the capture instead of switching to a default microphone.

Sink monitors are also selectable and explicitly labelled. They record a digital
output for routing diagnostics, not an acoustic room response. Saved reports mark
these takes as `sink_monitor_capture`. To choose where the test speakers play,
use **Studio → Audio Output → Output device**; Companion's selector controls
what it records. Neither choice changes the system-wide default device.

Without PipeWire, Linux falls back to CPAL/ALSA. macOS uses CoreAudio and Windows
uses WASAPI. ASIO and an electrical loopback channel are not implemented yet.
The CLI takes the exact `name` field from `capture devices` (for example
`pipewire:source:your-node-name`) and a one-based input channel:

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
  timing diagnostics/warning or rejection reason, impulse analysis when available,
  and the frame origin for analysis;
- `microphone-calibration.txt`, if supplied with `--calibration`: preserved
  two-column frequency/relative-dB data. It is validated but **not applied**.

PipeWire may resample to the chosen recording rate. Its metadata records that
continuity checking uses chunk flags and callback cadence, not device timestamps;
small discontinuities and all upstream clock behavior are not established by
this check. Hardware validation remains necessary.

Takes are rejected on stream errors, queue overflow, detected timing gaps, clipping,
missing or ambiguous sweep sequences, poor SNR or unstable reference levels. Limits
(20 dB SNR, 2000 ppm linear drift, 100 ms relative peak delay and 1 dB reference
level change) are prototype gates, not hardware accuracy guarantees.
Competing arrival peaks within an otherwise valid sweep sequence produce a
timing warning. Acquisition and level diagnostics remain available, while delay
and clock drift are withheld. Capture-report schema version 2 records
`timing.ambiguous_peaks`, the competing-peak ratios and background RMS; its
`relative_peak_delay_ms` and `clock_drift_ppm` fields are nullable.
`accepted_for_analysis` confirms acquisition checks, not validated arrival timing.
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

## Offline impulse-response review

```sh
cargo run --locked -p companion-cli -- capture review --take sessions/first-capture
```

The UI's saved-take review uses the same service. No microphone, audio output or
renderer connection is opened. A new `analysis-*.json` is saved beside the source
files; `recording.wav`, `stimulus.json`, `snapshot.json` and the original capture
report are never overwritten. The command returns success when review completes,
even if arrival timing remains unresolved; malformed or faulty acquisitions return
an error. Legacy takes rejected only for competing correlation peaks can be
reviewed. Interrupted, clipped, mismatched or otherwise rejected takes cannot be
promoted by reanalysis.

The analyzer reconstructs the emitted sweep, compensates its candidate constant
clock ratio and performs regularized, band-limited deconvolution. It retains three
impulse responses (reference, speaker, reference). The UI shows a peak-preserving
envelope and arrival markers for the broad, lower and upper analysis bands.
The report also retains the full band-limited response samples and their time axis.

An early local peak is tested across three frequency bands and detection
thresholds. Timing is withheld when those choices disagree or the repeated
reference waveforms do not match. A reported sensitivity bound describes those
choices; it is **not a calibrated accuracy or statistical confidence interval**.
An undetectable or obstructed direct sound can still leave a repeatable reflection:
no candidate is a validated direct-path observation. All reports retain
`valid_for_correction: false` and cannot be converted automatically into profiles.

This increment targets broadband speakers, not subwoofer alignment. Microphone
calibration, representative level estimation, nonlinear clock validation and
hardware accuracy assessment remain pending. See [the analysis method and
validation](docs/arrival-review.md) for the versioned algorithm, gates and limits.

![Saved-take arrival review with synthetic preview data](docs/images/arrival-review-preview.png)

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
- `crates/companion-cli`: command parsing and the simulated rig.
- `crates/companion-service`: discovery, readiness checks, background jobs,
  cancellation, acquisition orchestration and persistence shared by CLI/UI.
- `crates/companion-ui`: native egui views; no audio callback or protocol implementation.
- `crates/measurement-capture`: bounded input capture, calibration-file parsing
  and offline timing/impulse-response diagnostics; no correction policy.
- `crates/omniphony-adapter`: typed TCP requests, acknowledgements, session
  ownership and protocol validation; independent of the calibration core and UI.

The core accepts validated arrival/level observations; experimental arrival
candidates from recordings are kept separate until hardware validation. Application functions return a target
state: a future adapter must verify and apply it atomically in the renderer.
Profile application and restoration have no live network implementation yet.
The adapter's snapshots and render reports are separate from acoustic observation
sessions; renderer instance IDs are not permanent installation IDs.

The next steps are real-interface validation, arrival repeatability assessment
and representative-level analysis. The native assistant can already launch
individual takes; guided multi-speaker workflows, automatic iterations and
advanced corrections follow after that.

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
