# Continuous whole-system capture

Select an input, channel, sample rate and fixed reference speaker. Check the
speakers that are wired and ready, then choose **Measure selected speakers**. The
reference must remain connected even if it is unchecked as a measurement target.
Muted speakers and speakers whose frequency or level limits cannot accommodate
the selected sweep are excluded. Individual **Measure** actions remain available.

One renderer session plays `R, S1, R, S2, ..., R` into one uninterrupted input
recording. For N targets this uses 2N+1 segments instead of 3N. Every segment
includes the same 250 ms settling silence on each side of the sweep. A shared
reference is the same recorded audio in both neighboring results; no network
timestamp or replay stands in for an acoustic reference.

Progress appears inside the active speaker row. The toolbar remains present
throughout acquisition. Stop, including the active row's Stop button, cancels
the entire run. Keep the microphone, input gain, reference and renderer settings
fixed until recording ends. Settings and checkbox selections save automatically
and are restored on the next launch; startup never resumes a capture.

## Limits and results

Protocol 3 requires a matching renderer build. It permits up to 32 unique targets
and 120 seconds of rendered sequence. Companion additionally limits a batch to
4,000,000 mono capture frames, including a four-second startup/tail allowance and
clock-drift margin. At 48 kHz the overall budget is about 83 seconds. Reduce the
sweep duration, speaker count or input sample rate if the button's readiness
message reports this limit. Input capture and renderer callbacks remain bounded;
FFT analysis runs on the service worker after recording stops.

The outer references locate the sequence and estimate its overall clock ratio.
Intermediate references must also match that constant-clock model within 2 ms;
a discontinuity retains the raw batch without assigning speaker windows.
Each speaker's overlapping reference–target–reference window then passes through
the same local timing, clipping, level and impulse checks as a single capture.
A missing or clipped target can reject that speaker without rejecting unrelated
windows. A faulty shared reference can affect both neighbors. A capture fault,
cancelled playback or ambiguous outer reference alignment prevents extracting
speaker results, while retaining the raw batch and its failure report.

The batch directory contains:

- `recording.wav`: complete mono float recording, retained unchanged.
- `snapshot.json`, `batch-stimulus.json`, `batch-report.json`: identities,
  requested sequence, actual renderer progress and acquisition state.
- `batch-analysis.json`: derived take list, acceptance/rejection and batch errors.
- `take-001`, `take-002`, etc.: each speaker's normal take files plus a
  `sequence_window` in `stimulus.json` with its target index and original frame
  range. Stimulus schema 2 retains the actual whole-sequence renderer progress.

Offline review accepts a **take subdirectory**, not the batch root. Keep the
whole batch together when moving it: review verifies the parent progress and
compares the child's samples bit for bit with its source recording. Old
protocol-2 single-speaker takes remain supported.

Stop during playback saves a rejected partial batch. Stop during analysis keeps
the full raw recording and already finished results; it is checked between
speaker analyses. Force-killing the application or a disk failure can prevent
files from being saved. None of these results enables automatic correction.

## Validation

Deterministic tests cover segment routing, invalid later targets, lease expiry,
shared audio identity, capture/render rate mismatch and clock drift, saved-window
provenance, frozen selection, cancellation and stable UI row positions.

The `companion-service` example `batch_smoke` exercises a separately launched
renderer with two outputs routed to an isolated stereo PipeWire null sink. Its
selector must start with `pipewire:monitor:companion-batch-test-`. It records the
left monitor: the left target must pass acquisition, the absent right target
must fail, both must reopen for offline diagnostic review, and a second cancelled
run must retain raw audio and release session ownership. It is a digital bench,
not room or microphone validation.

Real installation testing should check speaker order, shared reference audibility,
repeatability against individual captures and Stop. Use a genuine listening
position before interpreting relative arrivals or levels.
