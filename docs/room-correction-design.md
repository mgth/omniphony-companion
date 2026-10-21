# Room-correction filter design

One correction filter per speaker, designed from the responses the
[room-correction measurements](room-correction-measurement.md) store: the main
seat's two passes and the guided positions around it. The filters are exported as
WAV files that Omniphony's Studio loads today in the speaker editor
(**Choose filter…**, a mono impulse response applied as is). Sending them over the
measurement protocol, verifying them by a new measurement and the bass/subwoofer
integration are later steps; nothing here changes Omniphony's settings.

The design is pure code in `calibration-core` (`room_correction`), reading nothing
but the samples it is given; `companion-service` (`room_correction`) collects the
stored responses of an alignment and writes the export.

## Use

After an alignment, and ideally the five other positions, **05 Correction filters →
Design and export filters** in the setup rail designs every speaker and saves the
result in a new `room-correction-<stamp>` folder of the alignment:

- `<output>.wav` for each corrected speaker: mono, 32-bit float, at the rate the
  renderer ran at during the alignment, 8192 taps;
- `room-correction.json`: the settings (target, limits, taps, rate) and, per speaker,
  its file or why it was not corrected, warnings, the largest boost (the headroom the
  filter needs), the deepest cut, the bands, the predicted deviation before and after,
  how the positions agree and the strong early reflections;
- `room-correction-curves.json`: per speaker, on a 1/48-octave grid over its measured
  band, the averaged response, the target, the correction asked, what the taps realise
  and the predicted corrected response.

The rail then lists each speaker with its predicted deviation from the target's shape,
before → after, or "not corrected"; hovering gives the reason and the warnings.
`omniphony-companion correction-filters --alignment DIR` does the same from the
command line, run from the folder the sessions were saved from, and accepts another
filter length, correction limit, tilt, boost and cut.

Load a filter in Studio, listen, and keep the level in mind: each filter boosts by at
most the reported headroom (3.5 dB by default), so content near full scale at those
frequencies can clip.

## Method

For each speaker:

1. **Responses.** Every accepted, unclipped take with a stored response: the main
   seat's two passes and each guided position's take. A take without a stored response
   (measured before they were kept) is left out and named in a warning. Each take is
   corrected by its own copy of the microphone calibration; takes with different
   calibrations, or some with and some without, are refused.
2. **Frequency-dependent window.** The response is read from 1.5 ms before its direct
   sound (the largest sample within 2 ms of the stored anchor) for 8 periods of each
   frequency above about 500 Hz, so later reflections, which no filter corrects for
   more than one point, stay out; below about 200 Hz the window is 300 ms, so room
   modes are resolved; it moves between them on a log scale. The window is flat for
   its first half and falls over the second.
3. **Spatial average.** Power is averaged over each position's takes, then over the
   positions, the main seat weighing 2 and every other position 1.
4. **Smoothing.** 1/24 octave below 300 Hz, 1/3 octave from 600 Hz, log-interpolated
   between. Every curve is levelled at 0 dB over 500 Hz–2 kHz as an alignment
   measures its level: mean power over the log grid.
5. **Diagnostics.** Each position's smoothed response is compared with the average
   above 300 Hz (RMS difference), the main-seat takes with each other, and the
   positions' spread is taken per frequency. Early reflections are listed from the
   main seat's first take: peaks of the 300 Hz–8 kHz envelope 0.7–25 ms after the
   direct sound, within 15 dB of it and standing 4 dB above their surroundings, with
   their delay, level and extra path at 343 m/s.
6. **Target.** Flat, falling 3 dB per decade above 1 kHz (−3 dB at 10 kHz), levelled
   like the response.
7. **Correction.** Target minus response inside the correction band: from a sixth of
   an octave above the sweep's start to 10 kHz (or a sixth of an octave below the
   sweep's end). Cuts reach −15 dB; boosts reach +3.5 dB, but none below the
   speaker's own −6 dB point (from the averaged response on a one-octave smoothing,
   going down from 500 Hz), ramping in over a third of an octave above it, and less
   where the positions disagree (full below 1.5 dB of spread, none from 4 dB): a dip
   that moves with the microphone is not filled. The limited correction is offset so
   the corrected response keeps its 500 Hz–2 kHz level, faded to 0 dB over half an
   octave outside the band and smoothed no finer than the taps can realise.
8. **Filter.** Minimum phase by the real cepstrum on a 65536-point grid, truncated to
   8192 taps with a raised-cosine fade over the last quarter, then scaled so the
   averaged response keeps its 500 Hz–2 kHz level: the level alignment stays valid.
   Its realised magnitude gives the predicted response and the headroom.

The excess phase above the bass and the reflections' combs are never corrected: the
feasibility study on the bench found the excess phase moving 40–180° between sessions,
and a reflection cancelled at one point coming back at −12 dB with a −8.5 dB pre-echo
five centimetres away.

A speaker is **not corrected** when its sweep misses 500 Hz–2 kHz (a subwoofer, left
to bass integration), when its band is too narrow, when calibrations differ, or when
its positions disagree too much: the median position more than 3 dB from the average
above 300 Hz. **Warnings** name positions more than 2 dB from the average, main-seat
takes more than 1 dB apart, two or more reflections within 3 dB of the direct sound,
a design from one position only, a missing calibration and takes left out.

## Validation

Unit tests on synthetic rooms (a speaker with a 2nd- or 4th-order roll-off, peaks and
dips, a floor reflection at 2.1–2.5 ms and a 90 Hz room mode, at a main seat and four
positions): the filter, applied to the main seat's response and measured again the
same way, brings the 250 Hz peak, the 3 kHz dip and the mode within 1–1.5 dB of the
target and the overall deviation to less than half; a dip deeper than the boost limit
stops at it; it is minimum phase (its energy builds up faster than the same taps
reversed); it keeps the 500 Hz–2 kHz power level; nothing is boosted below a 4th-order
speaker's −6 dB point (found within 10 %) and nothing changes outside the band;
disagreeing positions, a subwoofer and mixed calibrations are refused; the calibration
is subtracted. A service test runs the simulated rig through an alignment and two
positions into exported WAV and JSON files.

### Cross-validation on the bench

`cargo run --release -p companion-service --example room_correction_crossval -- SESSIONS_DIR`
recomputes the stored response of every take of saved alignments and, for each in
turn, designs from the others and checks on it. On the four alignments of
2026-10-20/21 (nine speakers, 1 s sweeps over 200 Hz–10 kHz, one microphone point, no
calibration), deviation from the target's shape over 226 Hz–8.85 kHz, RMS with its
mean removed, held-out alignment before → after, and the share of that band within
±2 dB:

| Output | Held out | Within ±2 dB | Predicted in design | One alignment → the others |
| --- | --- | --- | --- | --- |
| BC | 4.34 → 0.72 dB | 47 → 96 % | 4.31 → 0.70 dB | 4.34 → 0.72 dB |
| BR | 4.10 → 1.68 dB | 34 → 78 % | 4.04 → 0.63 dB | 4.10 → 1.69 dB |
| C | 3.97 → 1.79 dB | 40 → 87 % | 3.92 → 1.63 dB | 3.97 → 1.79 dB |
| FL | 4.50 → 2.83 dB | 35 → 86 % | 4.37 → 2.65 dB | 4.50 → 2.69 dB |
| FR | 4.36 → 2.71 dB | 34 → 88 % | 4.32 → 2.65 dB | 4.36 → 2.70 dB |
| TBL | 3.91 → 2.49 dB | 30 → 75 % | 3.85 → 2.38 dB | 3.91 → 2.49 dB |
| TBR | 5.32 → 1.09 dB | 20 → 93 % | 5.29 → 0.99 dB | 5.32 → 1.12 dB |
| spk-12 | 4.91 → 3.11 dB | 35 → 41 % | 4.85 → 3.00 dB | 4.91 → 3.11 dB |
| spk-13 | 3.23 → 2.16 dB | 57 → 91 % | 3.17 → 2.01 dB | 3.23 → 2.06 dB |

Held-out results match the prediction within 0.2 dB except BR, whose fourth alignment
differs from the others by 1.9 dB (the feasibility study found BR, BC and spk-12 less
repeatable). What remains after correction is mostly the deep floor-bounce nulls the
design does not fill (FL: −9 dB around 250 and 540 Hz, from its −4 dB reflection at
2.38 ms) and, for spk-12, a response whose early reflections are all within 2 dB of the
direct sound, which the design now warns about. Every filter's largest boost stayed
within 3.53 dB.

This is one microphone point measured four times, not five positions: it shows the
design is repeatable at a point, not that it holds around the seat. Nothing was
measured with a calibrated microphone or with the room-correction sweeps, no filter
was loaded in Omniphony, and nothing was listened to.
