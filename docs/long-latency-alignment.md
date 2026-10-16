# Long-latency alignment

Stable processing latency is a valid measurement. The 100 ms threshold now emits
an advisory in the UI; it does not reject correlation, arrival or repeatability
estimates. Signed relative delays are supported, including a slow reference.
The acquisition still requires an unambiguous reference–target–reference triple.
The target search covers ±45% of segment spacing, leaving an identity guard
before either reference. Longer routes may require a longer sweep/sequence;
this is separate from the latency warning.

The pure core accepts finite signed arrivals within its 10-second data bound.
Live proposal policy uses the saved renderer snapshot's `max_delay_ms`, including
existing settings. The default offline/simulator policy stays at 100 ms unless
explicitly changed. A renderer capacity failure blocks application, not measurement.
The adapter accepts finite advertised capacities from 0 to the existing 250 ms
sweep settling time. Larger future capacities need a longer/versioned stimulus.

The matching renderer allocates 250 ms output rings during stage construction,
using the same constant it advertises over OSC. At 48 kHz this is approximately
48 kB per output, up from 19.2 kB. Processing remains bounded and allocation-free;
the existing delay ramp is unchanged. No stored gain/delay is changed by building
or measuring. Application and verification remain explicit actions.

## Continuous batches

- Individual excerpts retain 200 ms margins, including genuine raw audio beyond
  the 150 ms impulse-analysis tail. Small anchor/clock rounding differences must
  not cause a false truncated-tail error.
- Noise is estimated over 100 ms ending 20 ms before the matched reference's
  sweep body. An excerpt's first samples may contain the preceding delayed
  target; they are no longer treated as guaranteed quiet background.
- Diagnostic target body windows follow a detected long-delay peak, rather than
  measuring only the nominal slot. Missing targets retain nominal windows.
- Every intermediate shared reference must match the global clock model within
  2 ms and meet correlation 0.2. A missing or displaced reference retains the raw
  batch and prevents splitting it into apparently valid takes. This conservative
  continuity gate detects a reference timing step; it does not identify whether
  playback, capture, buffering or a route caused it.

## Validation

Synthetic coverage includes ±160 ms arrivals with independent clock rates,
polarity changes, a repeated projector alignment through apply/verify/restore,
old-renderer capacity refusal without rejecting its evidence, consecutive delayed
targets with quiet-padding noise estimation, full impulse tails despite local
rounding, and a dropped-block reference discontinuity. Renderer tests check the
actual output-ring impulse at 160 and 250 ms and atomic OSC setting readback.
Real recordings remain subject to the same signal-quality gates. No raw hardware
recording is checked into the repository or rewritten during offline diagnosis.
