# Impulse-response and arrival review

This is the first analysis increment of mission 1. It produces inspectable arrival
candidates, not calibrated observations or gain/delay proposals. It runs offline
on the acquisition worker, never in an audio callback or UI paint call.

## Method: regularized-sweep-arrival-review-v2

1. Reuse the sequence anchors from `repeated-reference-correlation-v4`: a
   complete reference/speaker/reference sequence with bounded finite samples.
   Low SNR and unstable reference levels now produce quality warnings. Completed
   clipped recordings can be inspected but remain ineligible for correction.
2. Reconstruct the exact finite swept sine from the pinned renderer contract,
   including fades and settling silence. Resample that template with the
   candidate constant capture/render clock ratio from the reference spacing.
   A correlation ambiguity does not establish that this ratio is correct.
3. Extract each response around its rounded correlation anchor. FFT-deconvolve
   using `Y * conj(X) / (|X|² + 1e-5 * max(|X|²))`, with zero padding and explicit
   sample/FFT bounds. No frequencies outside the excitation band are recovered.
4. Apply full raised-cosine frequency windows. The broad band extends from
   `max(1.25 * start_hz, 300)` to `min(0.9 * end_hz, 9000)` Hz; two additional
   bands split it at 2500 Hz. The lower edge must be at most 1200 Hz and the upper
   edge at least 4500 Hz. Bass-only or insufficient-bandwidth sweeps are unsupported.
5. Form analytic impulse envelopes. Search from -50 to +100 ms relative to the
   correlation peak. A candidate must be a local envelope peak above ten times
   the pre-arrival RMS envelope noise. Repeat the selection at 20%, 30% and 40%
   of the strongest envelope peak, in each band. Retain the 30% broad-band peak
   only when all selections agree within 0.6 ms. The reported sensitivity also
   includes a two-over-broad-bandwidth time-resolution floor and a sample interval.
6. Compare the two reference impulse waveforms over -40 to +60 ms, searching
   ±1 ms around their candidate alignment. Require signed normalized waveform
   correlation of at least 0.90 and alignment residual at most 0.2 ms. Repeated
   reference agreement can be displayed even when the target remains unresolved.
7. Require the candidate-arrival clock ratio to agree with the template ratio
   within 100 ppm and remain within the existing 2000 ppm limit. Compute relative
   target arrival against the reference midpoint. Long stable route latency is
   advisory, rather than rejected at ±100 ms. Withhold
   relative arrival, drift and sensitivity bound if any required gate fails.

These are explicit prototype gates, not acoustic accuracy specifications. The
sensitivity bound combines target/reference band-and-threshold spread, the time
resolution floor and reference alignment residual; it is not a confidence level.

Full raised-cosine windows are intentional: a flat passband with narrow edge
fades produced pre-ringing sidelobes that the synthetic isolated-impulse test
mistook for early arrivals at the lower threshold. Frequency windowing itself
still limits temporal resolution. No threshold can prove that an earlier,
undetectable direct path is absent.

## Saved evidence and UI

New captures include an optional `impulse` report and `impulse_error` in the
capture-report schema 3. Correlation diagnostics keep their original
meaning and nullable timing fields. Impulse analysis failure does not turn a
successful acquisition into a failed capture.

Saved-take review validates schema/protocol, identities, reference sequence,
render completion, capture faults, WAV format/rate/length and analysis origin.
It recomputes from samples instead of trusting saved peaks. It accepts completed
acquisitions and completed low-SNR, reference-level-change, clipped or legacy
correlation-ambiguity rejections for diagnostic impulse analysis. Other completed
rejections can be inspected with [acquisition diagnostics](acquisition-diagnostics.md);
unavailable timing remains explicit. [Quality grades](measurement-quality.md)
separate inspectable results from correction qualification. Interrupted, faulty or inconsistent captures remain
refused. Analysis-report schema 2 retains the original acceptance and rejection
reason and makes timing optional. Sink-monitor provenance is
preserved and displayed as digital routing evidence, not an acoustic measurement.

Each invocation atomically writes a unique `analysis-*.json`, with no replacement
of source files. The report includes raw band-limited impulse samples, time axes,
band candidates, reference agreement, unresolved reasons and a bounded envelope
representation for display. It stores no automatic correction proposal.

Use **Review a saved take · no playback → Analyze saved take** in the UI or:

```sh
cargo run --locked -p companion-cli -- capture review --take sessions/example-take
```

The results panel lets you select reference-before, speaker or reference-after.
Its graph tabs also offer [frequency response and frequency-dependent timing](spectral-timing.md).
It also includes [signed waveforms and virtual bursts](virtual-bursts.md).
Plot time is relative to that segment's correlation peak; it is not absolute
propagation delay. Markers can remain visible while the combined arrival estimate
is withheld. The source microphone/position/reference are shown, without changing
the current acquisition settings.

## Validation and remaining limits

Deterministic tests cover fractional delays, polarity, different nominal capture
rates, positive/negative clock drift, a stronger late reflection, threshold-sensitive
weak arrivals, frequency-dependent arrivals, changing reference shapes/polarity, invalid
anchors and unsupported bands.
Service tests cover source-file preservation, repeat analysis, legacy reports,
monitor provenance and refusal of faulty/inconsistent takes. Headless UI tests
cover offline review, unresolved states and finite geometry at both window sizes.

Local recordings can exercise the complete pipeline without replaying sound;
this checks usability and consistency, not ground-truth direct-path accuracy.
Next bench checks are repeated same-position takes and known renderer delay
changes, followed by frequency-dependent speaker responses, obstructed direct
paths, clock discontinuities and nonlinear clock changes. Keep raw captures out
of Git. No new acoustic playback is part of the automated checks.

Remaining limits include finite sweep/tail windows, linear clock compensation,
linear interpolation of the template, unseparated nonlinear distortion and
band-dependent speaker group delay. A repeatable reflection can pass these gates.
Microphone calibration and absolute SPL are not applied. All results keep
`valid_for_correction: false`; profile generation/application remains separate.

The FFT is capped at 4,194,304 complex bins and captures at 30 seconds. Analysis
can still use hundreds of MiB at the highest supported rate and duration; 48 kHz
is the initial bench target. UI redraws use at most 1600 envelope points per curve
and share impulse reports instead of copying full responses per frame.

## Primary references

- [Novak — swept-sine deconvolution](https://www.ant-novak.com/pages/sss/):
  the frequency-domain input/output relation and reconstructing an impulse
  response from the emitted sweep. The companion's windowing, regularization and
  arrival-selection gates above are implementation choices, not a reproduction
  of the full synchronized nonlinear-identification method.
- [Farina — simultaneous impulse-response/distortion measurement (2000)](https://aes.org/publications/elibrary/elibrary-page/?id=10211):
  the underlying swept-sine measurement approach.
