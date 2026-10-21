# Companion application boundaries

The native frontend is a replaceable Rust/egui client of the same services as the
CLI. Keep acoustic policy separate from both presentation and acquisition.

| Crate | Responsibility |
| --- | --- |
| calibration-core | Pure observation validation, policy, proposals and verification |
| measurement-capture | Audio device access, bounded capture queue, offline timing and impulse-response analysis |
| omniphony-adapter | Typed renderer protocol, transport, ownership and acknowledgements |
| companion-service | Discovery, readiness checks, background jobs, capture orchestration, file persistence |
| companion-cli | Command parsing, console output and process cancellation |
| companion-ui | Native widgets, presentation state and user actions |

Do not add egui/eframe or another UI toolkit to the service, capture, adapter or
calibration core. Do not move audio callbacks, network requests or acquisition
keepalives into panel drawing code.

## Background work and ownership

The controller accepts one operation at a time. Input enumeration, renderer
discovery, measurement and saved-take review run on service workers; the frontend only consumes
events and sends actions. Each measurement owns a cloned request, snapshot and
settings. Editing a future input selection cannot alter a running take.

The cancellation token is shared directly with the worker. Stop and normal window
closure request cancellation; window closure waits for measurement cleanup.
The renderer retains its own lease/deadline protection if the process disappears.
The progress display does not drive the audio or transport clock.

The service's versioned `PreferenceStore` loads UI preferences before input
enumeration. The controller enumerates inputs and then discovers the saved local
renderer; it restores no snapshot, session ownership, progress or measurement
result. Selected input names, channels and reference IDs survive failed refreshes
and missing/muted devices. Live readiness checks remain mandatory.

Setup defaults live in the service (`companion_service::defaults`) and only fill
values that were never chosen. When no device is saved, the controller picks the
first input that is not a sink monitor and has a usable configuration (f32/i16/i32,
1–64 channels), with its preferred rate: 48 kHz when supported, otherwise the
supported rate closest to it, and channel 1. When no reference is saved, discovery
picks the speaker named C/Center (by ID or name, unmuted and able to play the sweep),
else FL/Front left, else the first speaker that can play the sweep. Empty microphone
and position names become `mic-1` and `main-seat`. A saved choice is never replaced,
even when that device or speaker is missing; the rail then opens its chooser with
the warning. `State::defaults` records which values were chosen automatically, and
the setup rail shows each chosen value with a **change** link (and "Chosen
automatically" when it applies). Editors stay open while a choice is missing and
close with **Done**.

Panels submit changed preferences to a bounded worker that keeps only the latest
value, debounces edits for 400 ms and publishes a synced temporary file by atomic
replacement. Normal window closure flushes pending edits. No filesystem writes
run in a paint callback or audio callback. Invalid or future-version files are
left untouched and disable saving for that launch; save failures are visible and
can be retried. Tests inject temporary stores; previews use no store at all.

A batch is one worker, one input stream and one renderer measurement session.
Its ordered output list is frozen at start; checkboxes affect future runs only.
The renderer schedules all segments without UI or network gaps between them.
The service saves the full recording and derives overlapping per-speaker windows;
shared references contain exactly the same raw samples. Panels draw progress in
a fixed-height speaker status row. Stop cancels the entire batch and retains the
partial raw recording; it never automatically starts another speaker.

The service estimates run time (`companion_service::duration`) from the sweep, the
speaker count and the passes: an input opening of 0.1 s, the 0.6 s metered lead-in,
the renderer sequence of 2 × speakers + 1 segments (sweep plus 2 × 250 ms settle),
up to 0.2 s of status latency, the 0.6 s tail, and offline analysis at 0.7 s per
second of take audio (measured on the session of 16 Oct 2026: nine 1 s sweeps
recorded in 30.1 s and analysed in 31.5 s per pass; machine dependent). The capture
loops use the same lead-in, tail and polling constants. The UI shows "about N s/min"
next to **Align levels and delays**, before Lab's diagnostic pass and in each card's
Measure tooltip; while a capture runs, `Controller::remaining` turns the job, pass,
speaker and latest progress into the time left shown in the run label and the
active card.

A new connection attempt discards the prior displayed snapshot immediately.
Measurement readiness checks the selected input format/rate/channel, reference,
speaker mute/band/level limits and metadata. The acquisition service performs
a fresh discovery check before opening the microphone and begins the session
against the exact snapshot. Stale configurations are refused.

## Results and preview

Each take has an independent directory. Renderer output names are not used as
path fragments. The UI retains the latest 100 take results in memory, associates
them with the measured speaker and settings, and keeps results from a changed
position/microphone/configuration out of the current speaker status. The files
remain available after closing the application.

Completed rejected takes can show reference/target acquisition diagnostics,
while retaining their original rejection. Completed weak or clipped takes may
also show diagnostic timing/impulse estimates with separate acquisition, timing
and level quality indicators; correction qualification remains separate. The
shared service owns reference-pair detection and classification; panels only
display the result.

Saved-take review has separate controller state. Saved analyses are indexed by
exact take folder and measurement identity so the matching speaker card and
individual result share the latest diagnostic status. Reanalysis does not change
acquisition settings or qualify corrections by itself. It requires no live
renderer/input. File validation, FFT work and unique report publication live in
the service worker; the UI draws bounded envelope data and shared impulse reports.
A reanalysis error remains visible when the saved-take panel is collapsed.

Live results and saved analyses share one selection in a resizable panel below
the speaker list. The individual result scrolls independently of the speaker list;
background completion cannot replace the selected speaker. Results never open
floating windows. Clicking a speaker card selects its latest result; nested
checkboxes and Measure/Stop controls retain their independent actions. The selected
speaker has a persistent tint and outline. Selecting an unmeasured speaker clears
the previous result and shows an explicit empty state. Quality badges share vector
symbols, colours, text labels and detail tooltips across rows and individual results.
Signed-waveform and virtual-burst traces are computed on the existing analysis
worker and saved in the report. UI frequency selection only chooses precomputed
traces, each bounded to 1600 signed min/max buckets. Panel clipping keeps speaker
cards from painting over the result panel; opening diagnostics scrolls internally.

The example preview installs a synthetic backend explicitly and labels the
window accordingly. It never records or plays audio. Production starts with
real input enumeration and no fabricated speaker list. The frontend never marks
a diagnostic take as a completed calibration.

## Validation

- Controller tests cover input constraints, single-job ownership, cancellation
  independent of painting, failed refresh, stale result contexts and unique paths.
- Headless egui interaction tests click input/channel selectors and per-speaker
  buttons, check unavailable states and verify the minimum window layout and
  unchanged speaker-row positions during single and batch acquisition.
- The preview is rendered through the native OpenGL frontend for visual review.
- Shared CLI, protocol, persistence and signal-analysis regressions remain active.

Hardware repeatability and a complete correction/apply/remeasure loop are still
pending; see the capture validation checklist.


## Experimental alignment

The alignment action owns two complete acquisition passes on a service worker.
Each target keeps its own repeated evidence. The pure core qualifies repeatability
and creates proposals; the capture crate estimates the common-band transfer level.
The alignment chart is painted from the bounded alignment rows and profile; a
UI-side summary derives relative values and maps raw reasons to short sentences
without changing service data. Each fixed-size speaker card shows the weakest
quality grade, measured values and the planned change. The action bar below the
cards emits Apply/Verify/Restore actions; Lab adds Reanalyze/Update and the raw
evidence. The entire speaker workspace scrolls within its panel. Card positions
depend only on the speaker count and width; while a new alignment is captured,
the previous chart's height stays reserved, so capture progress (inside the active
card) never moves the grid. Alignment never replaces the individual results in
the lower panel. The former alignment-tab preference slot now stores the Lab switch.

Profile application uses the adapter's absolute, revision-checked settings command.
The worker persists an uncertain state before mutation and confirms readback before
reporting success. Window closure waits for an in-flight application or acquisition;
no UI timer owns correctness. Loading a saved alignment does not touch hardware.
Verification uses the original settings and fresh repeated captures, clears old
verification success, and retains unresolved results. Configuration persistence
remains an explicit Studio action.

## Room map

The room map is a painter-only view in `companion-ui/src/room_map.rs`. It reads the
optional `azimuth_deg`, `elevation_deg` and `distance_m` of each discovered output
(measurement protocol 5): Omniphony's declared layout geometry, room-warped and at
the layout's scale (a normalised cube, not metres), never measured evidence. The
adapter validates their ranges with the rest of the snapshot and falls back to
protocol 4 when an older renderer refuses 5; outputs without an azimuth, and those
within 10° of straight above or below (LFE), go to an "Off plan" strip, and no map is
drawn when no output is placed. A missing distance puts the chip on a dashed ring at
the median known distance (three quarters of the radius when none is known).

Radii are horizontal: a distance `d` at elevation `el` is drawn at `d·cos(el)`. They
are the layout's until an alignment measured the speaker. Then
`companion_service::room::Distances` derives, for every qualified row, the acoustic
distance relative to the timing reference,
`Δd = c·[(arrival − delay) − (arrival_ref − delay_ref)]` with `c = 0.343 m/ms`, where
`delay` is the output's delay in the state the passes measured under
(`Alignment::measured_state`: zero for an alignment from zero; arrivals include it; a
row whose delay differs in any recorded retake state is left out). A row whose
delay-free arrival is more than 20 ms from the group's median is another output
device: it is drawn on the rim, distance unknown. The distances are anchored on the
reference by the least-squares fit to the layout's distances (the mean of
`layout − Δd`), or on a reference distance the user enters from the map's corner;
either is raised so no speaker comes closer than 0.1 m. The entered distance is a
display preference (`ViewPreferences::room_reference_m`), kept at once and never sent
to the renderer. Rings are labelled in metres only once a radius is measured, and a
hollow ring marks the layout's distance when it differs from the measured one by
more than 0.3 m. While a new alignment captures, the last measured distances stay,
so the chips do not fall back to the layout and jump out again.

Chip placement is a pure function of positions, measured distances, names and the
map size. Each speaker's true spot is marked (a dot at ear level, a triangle on the
height layer); outputs with the same layout position at the same spot share one chip
("BL·SR"). A greedy anchored pass places ear-level chips first, then heights: each
takes its own spot when free, else the nearest free place on rings of 16 directions
starting outward from the listener, never over another chip or the listener and, on
a first pass, not over another spot of its layer or of the ear level; a chip moved
off its spot keeps a leader line to it. A map too small for its chips falls back to
the nearest free place anywhere, then to a minimal-translation pass. Chips reuse the
card's quality dot, selection colours and click-to-select. Capture state only
changes how a chip is painted: the speaker playing pulses (the existing busy repaint
cadence drives it). Set-aside and excluded speakers are dimmed. Set aside means
unchecked, muted, outside the sweep band, or set aside by the alignment policy (a far
arrival not included anyway); global setup problems such as a missing microphone do
not dim speakers. The legend and the strip depend on reported positions only, so the
card's height never follows a measurement.

The map card takes a right-hand column beside the speaker cards; the chart joins the
left column only when it keeps at least 700 points, so its far-arrival bracket stays
readable, and otherwise spans the full width above. Its side follows the card grid
beside it (150–220 points), so it does not push the panels below the cards. A window
too narrow for two card columns beside the map shows it below the cards instead.
None of these choices depend on capture state, so starting a capture never moves the
chart, the map or the cards; interface tests check this, and that the cards keep
their place with and without positions.

Workspace restoration also belongs to the service: a background controller job
loads retained display reports and the last alignment, then enumerates inputs and
refreshes the renderer. The UI restores its selected result and graph preferences;
eframe owns persisted window/widget geometry. Cached speaker layouts remain separate
from live discovery and cannot authorize measurement or mutation. See
`workspace-restoration.md` for storage bounds and upgrade behavior.
