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
while retaining their rejection and withholding timing/impulse estimates. The
shared service owns reference-pair detection and classification; panels only
display the result.

Saved-take review has separate controller state and does not populate the current
speaker's measurement status or change acquisition settings. It requires no live
renderer/input. File validation, FFT work and unique report publication live in
the service worker; the UI draws bounded envelope data and shared impulse reports.
A reanalysis error remains visible when the saved-take panel is collapsed.

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
