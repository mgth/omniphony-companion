# Manual physical speaker levels

This tool matches active speakers by guiding adjustments to their physical
volume controls. It does not propose or write digital output gains or delays.
Use it before acquiring a new alignment session.

## Use

1. Connect to a renderer build with the optional finite level-noise extension.
   Select the measurement interface, microphone channel and reference speaker.
   Keep microphone position and interface input gain unchanged throughout.
2. Every speaker card has a **▶ Noise** button next to its status. It is greyed
   with the reason when the speaker cannot play the test noise (muted, outside
   500–2000 Hz, no microphone, or no reference level yet for a speaker other than
   the reference). Start with the conservative default test level (-30 dBFS); it
   is set under the meter and can only change while no noise plays.
3. Press **▶ Noise** on the reference speaker's card (marked REF). The renderer
   silences programme audio; the companion measures background, starts noise and
   waits two seconds for settling. The **Speaker levels** band appears below the
   cards with the live meter, and the button becomes **■ Stop**.
4. When the meter is live, choose **Use live level as reference** under it. The
   reference uses the current one-second band-power window. It is a snapshot, not
   a level that follows later knob movements.
5. Press **▶ Noise** on another card. A noise already playing stops first, then the
   new speaker plays. Adjust that speaker's physical volume. Negative difference
   means raise its volume; positive means lower it. Green indicates a difference
   within ±0.5 dB; the card of the speaker playing shows the same difference.
6. Repeat for each speaker. Capture a new reference if the microphone, its
   position, interface gain or any relevant setup changes. **Hide** closes the band
   once no noise plays; the alignment chart's **Match knobs** hint opens it too.
7. Acquire a fresh two-pass alignment after physical adjustments.

Each run stops after 60 seconds; the card's **■ Stop**, the global **Stop** button
and closing the app also stop it. Hiding the band does not own the timer.
There is no automatic playback on startup. The test-peak preference is saved;
reference levels and active runs are intentionally not restored after restart.

## What the meter means

The renderer emits one repeatable second of periodic pink noise, looped without
intervening gaps, with energy only in 500–2000 Hz. The microphone worker applies
fourth-order Butterworth high/low passes and calculates rolling one-second
power. It subtracts the quiet background power and removes the digital gains the
noise went through (the master gain, plus the output's gain when it was applied),
so the comparison reflects the physical volume controls. Those settings remain
unchanged.

## With or without Omniphony's level correction

By default the noise skips each speaker's own gain in Omniphony (measurement
protocol 6): every speaker plays at the same level, as if its correction were
0 dB, and you match the physical volume controls alone. The master gain still
applies. **Check with Omniphony's level correction**, under the meter, plays the
noise through that gain instead, and the difference then adds back each side's
gain: 0 dB means volume controls and correction together match the reference,
which checks the end result. The switch is off at every start and can only change
while no noise plays; a saved reference stays valid in both modes.

An Omniphony build older than protocol 6 always plays the noise through the
output's gain. The meter still removes it, and the band says so in a warning.
The displayed microphone band RMS is dBFS; the signed difference is a relative
comparison in dB. Neither is calibrated acoustic SPL.

The same stimulus and microphone setup must be used for reference and targets.
A change in renderer snapshot, endpoint, input/channel/rate, microphone/position
identity, reference speaker or noise peak invalidates the saved reference.
Changes to an interface's physical input-gain knob cannot be detected: capture
another reference after such a change. Do not move the microphone between speakers.

Reference saving requires a fresh live sample and at least 10 dB signal/background
ratio after background subtraction. Weak signal readings remain displayed with
a warning. Silence, missing frames, capture faults and clipping stop or prevent
playback. Stale UI samples cannot become reference evidence. The background
estimate remains fixed during a run; substantial changes in room noise require
restarting the run and checking the reference again.

The common band must fit each output's declared frequency range. Muted outputs,
sink monitors and subwoofers are excluded. This initial tool does not provide
bass matching, sensitivity calibration, multiple-microphone averaging or
automatic hardware volume control.

## Previous alignments

Physical knobs are outside the renderer's configuration revision. Before opening
an adjustment run, the companion conservatively marks the currently loaded
alignment stale and persists that state. It preserves the original digital
settings so an applied alignment can still be restored. Applying/verifying the
old proposal is blocked; offline reanalysis cannot make it current again.
Historical recordings remain available for inspection. Earlier saved sessions
also describe the old physical setup: acquire new evidence after knob changes.

## Validation

Automated tests cover known level ratios, background/trim compensation,
excited-band filtering, clipping/nonfinite samples, reference identity/freshness,
cancellation without frontend polling, persisted alignment retirement and digital
restoration, protocol identity/stop validation and UI adjustment direction.
Renderer tests cover spectrum, bounded storage, finite rendering, exclusive
ownership, unsafe output refusal, lease expiry, disconnect and setup changes.
A real renderer fixture that discards all audio buffers additionally exercises
finite completion, switching outputs, explicit stop and disconnect through the
Companion adapter. Real microphone/acoustic behavior still needs installation
validation; automated development checks do not play physical speakers.
