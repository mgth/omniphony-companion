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
| 3. Single-microphone acquisition | Audio input, microphone calibration file, level checks, raw capture, timing reference and independent clock handling. | Repeatability and quality are reported; available diagnostics survive poor quality, while clipped, interrupted, unsynchronized or excessively noisy evidence cannot qualify corrections. |
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
Individual takes remain diagnostic. The later repeated-alignment workflow below
connects qualified arrival/level evidence to experimental profiles.
See `docs/capture-validation.md` for the next bench checks.

A first milestone-6 interface now exposes the acquisition prototype in a native
Rust/egui window: input/channel selection, renderer speaker discovery, a fixed
reference, per-speaker Measure/Stop actions, progress and saved-take diagnostics.
The shared service owns discovery, capture and cancellation outside the UI event
loop. Whole-system acquisition now adds selectable outputs and a continuous
reference–speaker–reference sequence whose intervening references are shared.
Progress stays in the active speaker row. Acquisition preferences and speaker
checkboxes now persist locally, with startup device enumeration and renderer
reconnection. Startup also restores the last 100 result references, saved review,
alignment and graph/view choices; existing sessions are imported once when upgrading
from settings-only versions. Restoring a view never resumes playback or applies a
profile. See `docs/workspace-restoration.md`. Unavailable saved choices remain selected and block acquisition.
Automatic optimization iterations and guided microphone positioning remain pending.
See `docs/batch-capture.md`.

An initial milestone-4 increment now reconstructs band-limited impulse responses,
checks early arrival candidates across bands/thresholds and compares the two
reference waveforms. New takes and offline reanalysis share this code. The UI
can review a saved recording without a renderer and display impulse envelopes,
candidates and unresolved reasons. This remains experimental evidence, not a
validated direct-path observation or a standalone correction input. Repeated
alignment now adds representative levels and profile conversion. Microphone
calibration and hardware accuracy remain pending; see `docs/arrival-review.md`.

Spectral review now adds transfer magnitude, a denser band-energy arrival curve
and group delay from the same retained responses. These preserve a common time
origin per segment and expose weak/ambiguous regions. They support investigation
of multiway-speaker timing without claiming isolated driver delays or changing
correction eligibility; see `docs/spectral-timing.md`.

Signed impulse traces and virtual six-cycle tone bursts now expose oscillations
and relative phase without a new acoustic acquisition. Results share a resizable
panel below the speaker list. These inspection views do not change arrival
acceptance; independent burst capture and improved onset detection remain pending.
See `docs/virtual-bursts.md`.

A further acquisition increment diagnoses completed rejected takes using the two
reference sweeps independently of the target. Per-interval levels distinguish
clipping, missing target evidence and weak or uncorrelated sound. UI and CLI
retain the original rejection and do not convert these levels into correction
gains. Repeated alignment now adds a separate common-band transfer-level estimator;
hardware routing validation remains necessary. See `docs/acquisition-diagnostics.md`.

Experimental measurement quality now separates result availability from correction
eligibility. Excited-band SNR avoids unrelated out-of-band noise, and common-band
level SNR uses integrated power without dropping weak frequencies. UI and CLI
retain available low-confidence results with acquisition/timing/level grades.
Saved two-pass alignments can be reanalyzed into a separate diagnostic report;
source verdicts remain intact. Two-pass estimates and proposals now appear in each
speaker row, with session actions above the list; the lower panel stays dedicated
to individual measurements. See `docs/measurement-quality.md`.

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

## Current priority — Complete delay and level alignment

The experimental software loop now includes repeatable target acquisition, comparable band levels, conservative proposals,
explicit live application/readback, repeated verification and guarded restoration.
Real-interface accuracy, repeatability and acoustic before/after validation are
still required before calling mission 1 complete. See `docs/gain-delay-alignment.md`.
Input preparation now includes an explicit ten-second microphone meter and a fresh
pre-playback activity check on every capture. Digital silence is reported directly,
and alignment retains pass completion/failure summaries. See `docs/microphone-check.md`.
Long device latency is now advisory above 100 ms; live proposals use the renderer's
advertised capacity. Continuous batches retain a tail guard and check every shared
reference for timing discontinuities. See `docs/long-latency-alignment.md`.
Keep the microphone fixed at the reference seat. Preserve uncertainty: repeatability
alone does not establish an unobstructed direct path. Subwoofer integration remains
outside this mission. No tonal EQ or spatial sensor work should displace this priority.

## Deferred single-microphone backlog

The following work is retained for after mission 1:

1. Apply explicitly defined microphone amplitude calibration; early/full response
   windows and phase plots with a common time reference. Takes now keep their
   unweighted response and a calibrated magnitude for the design step
   (`docs/room-correction-measurement.md`); plots still use the review response.
2. Extend captures and impulse windows for bass decay, noise-aware decay fits and
   reflection timing diagnostics (without claiming arrival directions).
3. Guide successive microphone positions, show individual responses and spatial
   variation, and reserve independent validation positions. Five guided positions
   around the main seat are now captured and stored; display and validation remain.
4. Fit bounded parametric EQ to a target, favoring cuts; preview and remeasure.
5. Align speakers/subwoofers and optimize multiple subwoofers through gains,
   polarities and delays using their complex acoustic sum and shared timing.
6. Add minimum-phase FIR correction with a defined latency/runtime budget. Designed,
   applied over measurement protocol 8, compared and confirmed by measuring again
   (`docs/room-correction.md`); not yet run against a real renderer or listened to.
7. Evaluate excess-phase and modal-tail correction separately, only where repeatable
   improvements survive validation at other positions.

One movable microphone supports these tasks. Independent driver adjustments require
independent outputs. Direction, pressure-gradient and intensity measurements belong
to the later synchronous 3D probe work. Virtual bursts remain predictions from the
sweep response, not independent acquisitions.

## Later missions

- **Bass and subwoofers**: bass response, crossovers, source cooperation and
  seat-to-seat consistency within each speaker's capabilities. A first increment
  integrates one subwoofer with the speakers it serves: their parts measured through
  bass management at several positions, one delay, polarity and level for the
  subwoofer from the sums around each crossover, a cut-only mode correction, applied
  live and confirmed by a new measurement (`docs/bass-integration.md`). Several
  subwoofers, mixed-phase correction and validation on real hardware remain.
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


### Manual hardware level preparation

A separate manual-level tool now helps the user match physical active-speaker
volume knobs against a measured reference. It uses finite leased 500–2000 Hz
pink noise, a bounded microphone meter, background subtraction and relative
comparison with existing digital trims removed. It does not apply a calibration
profile. Current alignment proposals are retired before adjustments while keeping
original digital restoration data. See `docs/manual-speaker-levels.md`.
Installation validation, subwoofer matching and sensitivity-calibrated SPL remain
future work.


### Partial alignment proposals

Independent speaker-quality failures now exclude only those speakers. The pure
core requalifies the subset and checks its shared references; the service retains
all rows and captures, proposes only eligible outputs and preserves excluded
settings through application/restoration. Verification checks the fixed corrected
subset. The UI reports membership and exclusion reasons explicitly. Complete
original saved passes can update their proposal without replay or hardware writes;
diagnostic-only reanalysis remains non-applicable.

### Alignment chart view (UI phase 1)

The alignment is presented as a chart instead of numbers in speaker rows: a
Timing lane relative to the reference and a Level lane relative to the quietest
reliable speaker, each showing what Apply changes. Compact speaker cards, one
action bar (Apply, Original/Corrected, Confirm by re-measuring, Undo) and short
human exclusion reasons follow. A persisted Lab switch keeps every previous
diagnostic one click away. This phase changes presentation only; services,
policy, persistence semantics and renderer writes are unchanged.
