# Experimental measurement quality

A completed recording now exposes every available result even when its SNR is
weak. Availability, quality and eligibility for a gain/delay correction are
separate. Missing signals or synchronization still mean unavailable timing;
the software does not manufacture a delay from noise.

## Reading the indicators

The result panel shows separate **Acquisition**, **Timing** and **Level** grades.
Hover over a grade or expand **Measurement quality details** for measured ratios,
faults and unresolved reasons. Alignment rows retain estimates from each pass
even when they cannot qualify; their Quality column shows the weakest grade.

| Grade | Meaning |
| --- | --- |
| Good | Available evidence meets the prototype SNR checks. |
| Limited | Inspectable evidence has marginal SNR or weak spectral regions. |
| Poor | Very noisy evidence, clipping, unstable references or unresolved arrival checks. |
| Unavailable | The necessary evidence or supported frequency coverage is missing. |

These are heuristic indicators, not confidence probabilities or calibrated
accuracy. A repeatable reflection can still be mistaken for the direct path.
A Good single take alone cannot authorize a correction.

Acquisition uses the minimum SNR of reference-before, target and reference-after:
20 dB or more is Good, 15 to below 20 dB is Limited, below 15 dB is Poor. Clipping
or reference RMS changes above 1 dB mark it Poor. Timing inherits acquisition
quality only when the impulse analysis finds a supported arrival candidate;
otherwise its unavailable estimate and analysis failures remain visible.

Level uses the integrated SNR over 500–2000 Hz, with the same grade boundaries.
Any level-grid neighborhood below 20 dB caps its grade at Limited. The panel
retains the weakest frequency SNR and the number of affected grid points.
Clipped or otherwise faulty captures have Poor level quality even if a numerical
estimate exists. A relative transfer level is not calibrated SPL.

## Noise and frequency weighting

`repeated-reference-correlation-v4` and `reference-pair-acquisition-v3` add
`band_noise` without replacing the original broadband RMS and SNR fields. A
fourth-order Butterworth high pass at the sweep start and low pass at the sweep
end (bounded to 0.45 times the effective capture rate) filter a temporary copy.
The same filter measures each sweep body and 100 ms of quiet audio ending 20 ms
before the first reference body. Correlation and impulse analysis continue to
use the original PCM. This prevents unrelated low-frequency rumble or ultrasonic
noise from dominating an otherwise useful sweep-band ratio. The filters have
finite transition bands; this is an estimate, not a brick-wall noise integral.

Ratios are (signal + noise) / noise, without subtracting noise from the captured
power. Short or nonstationary background noise can bias them. The UI explicitly
labels the retained unfiltered ratios as broadband.

The relative level estimator still uses all 25 points on the same logarithmic
500–2000 Hz grid and the same trapezoidal power weights for every speaker.
Integrated noise power is reconstructed from the spectral magnitude and its
colored-noise proxy with identical interpolation and weights. A local dip no
longer rejects the complete level estimate. Weak points are never dropped or
reweighted to make a speaker appear louder. The level method ID is unchanged
because the measured quantity and weighting are unchanged; quality uses
`experimental-band-quality-v1`.

`windowed-transfer-and-band-energy-v2` retains supported finite magnitude and
group-delay values at low SNR. Points below 20 dB are amber and do not join the
main trace. Deep-null and extreme group-delay bounds remain. Peak-energy arrival
keeps its signal/ambiguity checks. Phase dominated by noise is inspectable, but
is not evidence of a physical driver delay.

## Correction qualification

Displaying a value does not qualify it. Repeated alignment additionally requires:

- Complete, consistent, unclipped acquisitions and supported arrival candidates.
- At least 15 dB excited-band acquisition SNR in all three sweep segments.
- At least 20 dB integrated level-band SNR for the target and both references.
- At most 1 dB reference sweep-RMS change and 0.50 dB transfer-level change.
- Two independent target captures agreeing within 0.15 ms and 0.50 dB.
- Existing waveform, clock, identity, renderer revision and correction bounds.

Every selected speaker must qualify; unresolved outputs are not silently omitted.
These thresholds are experimental policy and can be revisited with bench evidence.
No automatic gain increase, playback or correction is introduced by this feature.

## Existing recordings

Loading saved state does not silently rerun analysis. Use **Reanalyze this take**
for one result, or **Reanalyze saved passes** in the alignment session controls
above the speaker rows for both passes.
The corresponding commands are:

```sh
cargo run --locked -p companion-cli -- capture review --take sessions/example-take
cargo run --locked -p companion-cli -- capture review-alignment --session sessions/example-alignment
```

Completed legacy SNR, reference-level and clipping rejections can receive new
diagnostic results. Their original acceptance flag and reason remain in the
review. Interrupted, nonfinite, identity-mismatched or inconsistent acquisitions
cannot be repaired by lowering a threshold.

Reanalysis creates unique `analysis-*.json` files and, for a whole alignment, a
separate `alignment-review-*` directory. It preserves all original WAVs, capture
reports and alignment state. The derived alignment is diagnostic only: it has
no application profile and never contacts the renderer. Applied or uncertain
sessions cannot be replaced this way, keeping restoration available. Capture a
fresh alignment to apply and acoustically verify a proposal.

Old reports remain readable through optional additive fields. Without a saved
excited-band estimate, the UI marks legacy broadband-only quality as Limited
at best and suggests reanalysis. No hidden migration rewrites source evidence.

## Validation

Deterministic tests cover out-of-band noise, weak spectral regions with unchanged
weights, clipped diagnostic responses, retained legacy verdicts, missing signals,
separate correction bounds, immutable reanalysis and UI navigation without audio.
Real-session reanalysis can demonstrate repeatability and changed classification;
it cannot establish hardware accuracy or replace controlled delay/gain tests.
