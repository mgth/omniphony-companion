# Development plan — Omniphony Companion

## Goal

Build an Omniphony companion that automates acoustic measurement and system
calibration using one or more microphones:

**Prepare → measure → analyze → propose → apply → remeasure → verify.**

The companion owns sessions, acquisition, analysis and optimization. Omniphony
plays measurement signals and applies settings to physical outputs. Advanced
correction methods remain open: this plan defines responsibilities and
milestones without prematurely choosing bass or reflection algorithms.

## Mission 1 — Delays and levels

### Expected outcome

The user connects a microphone, selects a reference position and starts
measuring the speakers. The companion produces gain and delay settings verified
through a new acquisition. The user can compare before/after, explicitly save
the result or restore the previous settings.

Levels are relative. Absolute sound pressure measurements require appropriate
sensitivity calibration and are not a prerequisite for this first mission.

Delays are aligned for one reference position. Other positions help assess
robustness and, later, determine a listening-area compromise. Identical arrival
times throughout the room are not promised.

Subwoofers remain separately identified. Their measurements and settings are
preserved; their acoustic integration with the speakers belongs to the bass
mission. A subwoofer response peak must not automatically be interpreted as the
direct arrival of a full-range speaker.

### Development milestones

| Milestone | Work | Acceptance criteria |
| --- | --- | --- |
| 1. Foundation and sessions | UI-independent core; models for installations, microphones, positions, measurements and profiles; versioned persistence; CLI and simulated test rig. | A session can be saved, reloaded and analyzed without a renderer or audio hardware. |
| 2. Omniphony contract | Discover physical outputs and their limits; capture initial settings; emit finite, identified signals; acknowledge commands; start, finish and stop sessions. | The correct output emits the correct signal, and the renderer independently stops an abandoned measurement. |
| 3. Single-microphone acquisition | Audio input, microphone calibration file, level checks, raw capture, timing reference and independent clock handling. | Repeatability is measured; clipped, interrupted, unsynchronized or excessively noisy acquisitions are rejected. |
| 4. Delay/level analysis | Impulse response, direct-arrival and representative-level estimation, uncertainty indicators and bounded setting proposals. | Validation against known signals and a real installation; ambiguous arrivals are detected and reported. |
| 5. Application and verification | Apply a consistent profile, read back actual settings, acquire again, compare results and make bounded adjustments. | New measurements confirm the result; reapplying a profile does not accumulate corrections. |
| 6. User assistant | Preparation, progress, targeted retries, before/after results, level-matched listening comparisons, saving and restoration. | The complete workflow requires no manual commands; closing or minimizing the window does not affect stop deadlines. |
| 7. Multiple positions and microphones | Guided microphone repositioning, then simultaneous capture through a multichannel interface; calibration and results for each microphone/position. | The same data and analysis support a moving microphone and fixed microphones, with an explicit alignment reference. |

These are functional milestones, not a requirement to finish each milestone
before testing a complete workflow. The first software increment covers
milestone 1 and exercises proposals and verification using simulated
observations. It does not complete real acquisition or the acoustic analysis
in milestones 3 and 4.

The second increment implements milestone 2's software path: a shared versioned
TCP/OSC contract, output discovery and snapshot revisions, an exclusive expiring
session, finite sweeps, explicit stop/end and render-progress acknowledgements.
The companion CLI exercises it and saves stimulus metadata and render reports.
It is validated against real renderer code with discarded output buffers;
physical output routing and acoustic behavior still require installation tests.
The third increment adds selected-channel interface capture, retained float WAVs,
calibration-file preservation and repeated acoustic reference sequences.
Offline diagnostics estimate linear clock drift and relative correlation-peak
delay and reject several capture/quality failures. This is a milestone-3
prototype: real-interface repeatability and discontinuity tests remain open.
It does not yet produce direct-arrival observations or calibration profiles.
See `docs/capture-validation.md` for the next bench checks.

A first milestone-6 interface now exposes the acquisition prototype in a native
Rust/egui window: input/channel selection, renderer speaker discovery, a fixed
reference, per-speaker Measure/Stop actions, progress and saved-take diagnostics.
The shared service owns discovery, capture and cancellation outside the UI event
loop. Whole-system acquisition now adds selectable outputs and a continuous
reference–speaker–reference sequence whose intervening references are shared.
Progress stays in the active speaker row. Acquisition preferences and speaker
checkboxes now persist locally, with startup device enumeration and renderer
reconnection. Unavailable saved choices remain selected and block acquisition.
Automatic correction and guided microphone positioning remain pending.
See `docs/batch-capture.md`.

An initial milestone-4 increment now reconstructs band-limited impulse responses,
checks early arrival candidates across bands/thresholds and compares the two
reference waveforms. New takes and offline reanalysis share this code. The UI
can review a saved recording without a renderer and display impulse envelopes,
candidates and unresolved reasons. This remains experimental evidence, not a
validated direct-path observation or an automatic correction input. Representative
levels, microphone-calibration application, hardware accuracy and profile
conversion remain pending; see `docs/arrival-review.md`.

A further acquisition increment diagnoses completed rejected takes using the two
reference sweeps independently of the target. Per-interval levels distinguish
clipping, missing target evidence and weak or uncorrelated sound. UI and CLI
retain the original rejection and do not convert these levels into correction
gains. Hardware routing verification and representative-level estimation remain
pending; see `docs/acquisition-diagnostics.md`.

### Multiple microphones

- Start with one microphone and guided measurements at successive positions.
- Model every observation by speaker, position and microphone from the outset.
- Then add multiple synchronized inputs on a shared interface.
- Defer independent USB microphones until per-device offset and clock-drift
  compensation has been validated.
- Preserve individual observations; averaging raw signals does not replace an
  explicit acoustic aggregation method.

### Measurement and application rules

- Separate command transport from audio timing: an OSC message arrival does not
  identify the emission time of an audio sample.
- Preserve a shared timing reference across compared measurements without
  removing the propagation differences being measured.
- Fix and record gains, processing and routing throughout acquisition.
  Avoid automatic gain changes and microphone processing.
- Use stable output identifiers; detect changes to the layout, sample rate or
  settings during a session.
- Distinguish initial settings, calculated corrections and absolute targets to
  prevent accumulated corrections and application to a different installation.
- Apply settings without allocating or running analysis in the audio path;
  prepare data outside realtime processing.
- Bound signal levels/durations, gains and delays. The renderer must stop
  locally if the companion disappears, independently of the UI.
- Stop iterating when improvements are no longer significant relative to
  repeatability. Retain the best verified state.
- Distinguish preview from saving: temporarily applying a profile must not
  silently change Omniphony's persistent configuration.

## Architecture

1. **Calibration core**: models, validation, session progression, adjustment
   policies and result comparison. No UI dependencies, hardware access,
   threads or OSC addresses.
2. **Acquisition and analysis services**: capture, signals, synchronization and
   recording analysis; work outside realtime processing, with traceable results.
3. **Omniphony adapter**: typed commands, confirmed state, configuration checks
   and consistent profile application/restoration.
4. **UI and CLI**: drive the same operations. The UI presents data and actions;
   services own timers and ongoing operations, even when the window is minimized.

Implementation starts in an independent Rust workspace. Analysis modules can
evolve without changing the acquisition model. A plugin system is not required
at this stage.

Omniphony already provides per-speaker gains/delays and a pink-noise output
test. Synchronized acquisition needs a dedicated contract: the existing test
alone is not a calibration protocol.

## Data to retain

- Installation configuration, speaker roles and identifiers, routing, sample
  rate and the settings actually used.
- Microphones, individual calibration, positions and the reference position.
- Emitted signals, raw recordings, timing reference and acquisition metadata,
  followed by analysis method versions.
- Observations, quality/uncertainty, proposed settings, applied settings and
  verification results.

Schemas are versioned. Recordings and calibrations can become associated session
files, portable without machine-specific paths. The initial schema stores
already-estimated observations and must never present them as microphone
captures.

## Validation

Three complementary levels:

1. Deterministic tests: known offsets/gains, invalid input, missing measurements,
   poor quality, configuration changes and reapplication.
2. Audio measurement bench: timing reference, clock drift, noise, clipping and
   reflections that could obscure the direct arrival.
3. Real installation: repeatability, before/after deviations, control positions,
   listening comparisons and interruption/reconnection tests.

Acoustic tolerances will be chosen using bench results and measured
repeatability. Simulator thresholds are not a promise of real-hardware accuracy.

## Later missions

- **Bass and subwoofers**: bass response, crossovers, source cooperation and
  seat-to-seat consistency within each speaker's capabilities.
- **Reflections and temporal behavior**: identify reproducible, correctable
  phenomena, then evaluate corrections across the listening area and by listening.
- **Overall refinement**: target curves, preferences, listening-area compromises,
  profiles and optimization strategies informed by experimental results.

These missions reuse acquisition and the verification loop. Their algorithms
and IIR/FIR/MIMO choices will be decided separately.

## Delivery and tracking

- Use one `feat/*` or `fix/*` branch per concern and a PR with green CI and review.
- Do not develop features directly on `main`.
- Any Omniphony changes get their own workflow and PR targeting `dev/calibration`;
  do not implement them in the integration worktree or target `main` for this work.
- Keep this plan in the companion repository. `README.md` describes the
  executable capabilities and prototype limitations accurately.
- Use English exclusively for repository content and project communication,
  as specified in `AGENTS.md`.

## Starting references

- [Cecchi, Carini, Spors — Room Response Equalization review (2018)](https://arts.units.it/handle/11368/2933738)
- [Farina — swept-sine measurement (2000)](https://aes.org/publications/elibrary/elibrary-page/?id=10211)
- [Kirkeby and Nelson — regularized inversion (1999)](https://aes.org/publications/elibrary-page/?id=12098)
- [Welti and Devantier — multiple-subwoofer optimization (2006)](https://aes.org/publications/elibrary/elibrary-page/?id=13680)
- [REW — measurements and timing references](https://www.roomeqwizard.com/help/help_en-GB/html/makingmeasurements.html)
- [DRC-FIR — method documentation](https://drc-fir.sourceforge.net/doc/drc.html)
- [CamillaDSP — engine and documentation](https://github.com/HEnquist/camilladsp)
