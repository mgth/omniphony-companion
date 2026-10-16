# Frequency response and frequency-dependent timing

New captures and **Reanalyze this take** provide four result views: Impulse,
Frequency response, Arrival / frequency and Group delay. The reference-before,
speaker and reference-after selector applies to all four. Old accepted takes can
be reanalyzed without playback. Completed low-SNR and clipped takes can also
produce diagnostic curves while retaining their original verdict and visible
quality warnings. Interrupted or inconsistent captures cannot supply timing anchors.

## Reading the plots

**Frequency response** is the captured/source transfer magnitude, displayed with
its largest available point at 0 dB. Saved points retain the unnormalized digital
amplitude ratio in dB. It includes the speaker, room, microphone, interface gain
and existing renderer gain. It is not SPL, microphone-corrected or anechoic data.
Only the usable excited range is plotted; a 200 Hz–10 kHz sweep cannot describe
the bass below 200 Hz. Frequency uses a logarithmic axis.

**Arrival / frequency** plots the strongest energy-envelope peak in successive
overlapping 1/3-octave bands, sampled at 12 centers per octave. This can reveal
different arrival regions across a multiway speaker's frequency range. It is
not the earliest/direct arrival detector used in the existing three-band table.
Amber points have another similarly strong, separated peak and are not joined
into the trace. A reflection can be the strongest peak. Narrow low-frequency
bands have poor time resolution: hover shows the approximate filter time width,
which is not a statistical uncertainty or accuracy bound.

**Group delay** is the local phase-slope delay, with energy weighting over a
1/12-octave neighborhood. Crossover phase, resonances and reflections influence
it along with physical delay. It can be negative in some ranges; that is not
evidence of an acausal speaker. Points below 20 dB estimated spectral SNR are
amber and are not joined into the main trace. Deep nulls and extreme values remain withheld. An amber phase
value can be dominated by noise and is not a supported physical delay.

Both timing curves use the **same correlation-peak zero for the selected
segment**. No per-band alignment flattens the differences. Positive values mean
later energy; values are not absolute propagation time or direct driver distance.
Changing segments changes the anchor, so these plots are not an inter-speaker
time alignment overlay. The existing reference-relative arrival estimate keeps
its own gates and meaning.

These views are useful evidence for investigating drivers, but do not identify
them independently. To attribute an offset to particular drivers, use a suitable
microphone position and controlled driver/crossover measurements. The fixed
response window includes reflections; selectable early windows and a full
time-frequency map remain possible later additions.

## Offline method: windowed-transfer-and-band-energy-v2

The original regularized sweep deconvolution and synchronization checks are retained;
SNR controls [quality indicators](measurement-quality.md), rather than hiding responses.
For each segment, recover the real impulse response **before** the arrival
detector's raised-cosine frequency bands. Applying those bands first would distort
the magnitude plot. Retain -100 to +150 ms relative to the rounded correlation
anchor and taper the outer 5 ms at both ends. FFT padding is at least twice this
window, capped at 131,072 bins. Curves have at most 128 points; paint callbacks
draw saved points and perform no signal analysis.

The usable range is 1.1 times the sweep's start frequency to 0.9 times its end,
also bounded below 0.44 times the clock-compensated sample rate. Magnitude uses
the mean FFT power within 1/12 octave. Let `H = FFT[h]` and
`T = FFT[(n - anchor) h]`. Group delay is
`sum Re(T conj(H)) / sum |H|² / sample_rate`, converted to milliseconds.
This is an energy-weighted phase derivative without unwrapping across nulls.
Subtracting the common anchor removes extraction latency, not per-band delays.

A colored-noise proxy uses the spectrum of the Hann-windowed pre-arrival impulse
before -60 ms, averaged over 1/3 octave and scaled by the ratio of the full and
quiet windows' squared weights. Finite supported magnitude and group-delay values
are retained below 20 dB spectral S/N and displayed in amber. Peak-energy arrival
still requires 20 dB and power above -60 dB relative to the largest FFT power.
Group delay requires every bin in its local neighborhood to exceed 1% of that
neighborhood's mean power and is withheld outside ±100 ms. These are prototype
quality indicators and display bounds, not coherence or hardware accuracy measures.

Peak energy uses positive-frequency analytic responses with full raised-cosine
1/3-octave bands. Search -50 to +100 ms; withhold boundary peaks and peaks below
ten times their band's pre-arrival RMS envelope. Refine the strongest peak with
three-sample parabolic interpolation. A second local peak above 80% of the
maximum and separated by more than `2 / bandwidth` is marked ambiguous. Complete
band support inside the usable excitation range is required.

Spectral reports are additive optional fields on each impulse segment, with a
separate method identifier and optional failure reason. Their availability does
not change capture acceptance, arrival validity or `valid_for_correction: false`.
Broadband arrival analysis is still a prerequisite in this increment; bass-only
capture/analysis is not enabled by adding these plots.

## Validation and references

Tests cover flat gain, polarity, signed pure delays and different sample rates;
analytic first-order filter magnitude/group delay; a delayed high-frequency
branch; competing echoes, noise-only data and invalid windows. End-to-end saved
take tests check absolute digital transfer level and removal of the extraction
offset while preserving original files. UI tests cover all views, masked points,
tab switching and finite geometry at both supported window sizes.

- [REW group delay](https://www.roomeqwizard.com/help/help_en-GB/html/graph_groupdelay.html)
  explains the phase derivative and effects of measurement time zero.
- [REW spectrogram](https://www.roomeqwizard.com/help/help_en-GB/html/graph_spectrogram.html)
  describes peak-energy timing, the time/frequency resolution tradeoff and
  inspecting drive-unit alignment. This implementation is a bounded filter-bank
  curve, not a reproduction of REW's wavelet spectrogram.
