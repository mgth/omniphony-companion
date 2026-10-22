# Subwoofer integration

Omniphony's bass management splits every source into bands before panning: a band
below a speaker's `freq_low` goes to the outputs whose range covers it, usually the
subwoofer. Around that crossover the speaker and the subwoofer play together, so their
arrival times, polarities and levels decide whether they add up or cancel. The
alignment cannot time a subwoofer (its sweep shows no broadband arrival) and its level
band, 500 Hz–2 kHz, is out of the subwoofer's range. This step measures how the
subwoofer adds up with each speaker it serves, proposes one delay, polarity, level and
mode correction for the subwoofer, applies them live and confirms them by measuring
again.

The analysis is pure code in `calibration-core` (`bass`, and
`room_correction::design_subwoofer` for the mode correction); `companion-service`
(`bass`) plans and runs the measurements, writes the analysis and applies it through
measurement protocol 8.

## Use

The subwoofer is part of the guided [room correction](room-correction.md) step, in the
setup rail's **04 Room correction**, after an alignment (applied: the subwoofer is timed
against the speakers' delays as Omniphony has them, and a later Apply or Undo of the
alignment makes these measurements stale):

1. **Measure the subwoofer** with the microphone at the main seat. A quiet level probe
   first, then every part described below. About a minute per group of speakers.
2. **Measure position 1** to **5** measure the subwoofer too, once it was measured at
   the main seat: the same at the guided positions around the seat (30 cm left, right,
   forward, back, 15 cm higher), which the proposal averages, the main seat counting
   twice.
3. Read the proposal on the subwoofer's card (delay, level change, polarity, mode
   correction) and, in its drawer, how far each speaker's sum around the crossover
   falls below the two adding in phase, measured and proposed.
4. The room correction's **Apply** sends the subwoofer's filter in the same
   `set_output_filters` as the speakers' filters, then its delay and level with
   `set_outputs` (and, when it would need less than no delay, the same added delay to
   every other output). Live only: they reach `config.yaml` only through Studio's
   Save. **Undo** puts the subwoofer back as measured, without its filter. Turning
   **Integrate the subwoofer** off in Lab's correction settings leaves it as it is.
5. **Confirm by re-measuring**, with the microphone back at the main seat, also
   measures every speaker's sum again through the filters and compares it with the
   prediction.

`omniphony-companion bass-integration --alignment DIR` analyses saved measurements
again offline and writes a new `bass-integration-<stamp>` folder; nothing is sent.

## What is measured, and why this way

The renderer only plays a direct sweep inside an output's declared band: the speaker
from its `freq_low`, the subwoofer up to its `freq_high`. Neither can be swept alone
across their crossover. Protocol 8's **bass-managed** sweep plays a source at the
speaker's position through the crossover and the panning, as programme audio does, so
the subwoofer gets the low band. Each speaker is measured with it in three states, the
outputs being written to −60 dB (the lowest gain the renderer accepts) for one sequence
and put back right after:

| Part | State | Gives |
| --- | --- | --- |
| together | as it is | the sum |
| subwoofer silenced | the subwoofer at −60 dB | the speaker's part (and any other speaker its low band reaches) |
| speakers silenced | every output but the subwoofer and the timing reference at −60 dB | the subwoofer's part |
| subwoofer's own sweep | as it is, direct | the subwoofer's response for its mode correction |

Every session begins with `bypass_output_filters`: existing correction filters do not
colour the parts. The subwoofer must have no filter of its own (Undo could not put a
file filter back through the protocol), and there must be exactly one unmuted output
whose range stops below 5 kHz.

**Time base.** The reference speaker's direct sweeps frame each target. A stored
response now keeps the target's correlation peak relative to the midpoint of the two
reference peaks (`target_offset_frames`), so every take at a position shares the
reference's arrival as time zero, whichever sequence played it. Takes are lined up
once more by cross-correlating their reference sweeps with the sum's, in case a
correlation peak was picked differently.

**Groups.** The reference must play the whole sweep, and the sweep must reach half
each crossover. The reference is the speaker reaching lowest (the alignment's own when
it reaches as low); a speaker split lower than it can frame (on a layout where every
speaker stops at 40 Hz, the 40 Hz speakers) is measured in a second group framed by
the subwoofer's own direct sweep, over the subwoofer's range. Silencing the subwoofer
would silence that reference, so those speakers are measured together and with the
speakers silenced; their own part is the sum minus the subwoofer's. Likewise the main
reference keeps playing when the speakers are silenced: its subwoofer part is the sum
minus its own. The measured sum checks the parts only where both were measured apart.
A speaker no group can sweep an octave wide around its crossover is listed and left
out.

## Analysis

Each part is transformed on the position's time base over the window all its parts
share (about 0.4 s, 4 Hz resolution), on a 1/48-octave grid from a little below half
to a little above twice the crossover; powers are smoothed over 1/12 octave.

1. **Timing from the phase.** The cross-spectrum of the subwoofer's part and the
   speaker's, smoothed as a complex value, unwrapped from the crossover outward, is fit
   with a weighted straight line over the octave around the crossover: its slope is how
   much later the subwoofer arrives, its offset whether they are in opposite polarity.
   This includes the drivers' own phase there (a room mode near the crossover adds its
   group delay), which is what the sum hears.
2. **Checks.** The subwoofer's share of the low band (its part against the speaker's
   own, half an octave below the crossover): below −6 dB its bass goes to other
   speakers and the speaker is left out. The sum measured against the parts added: more
   than 1.5 dB RMS is named, more than 3 dB leaves the speaker out (something changed
   between the measurements).
3. **Mode correction.** The subwoofer's own sweeps, windowed over 300 ms, power-averaged
   over the positions (the main seat counting twice), smoothed over 1/24 octave and
   levelled at their mean over the band: a minimum-phase filter cuts what stands above
   that level (at most 12 dB, no boost), faded to 0 dB over half an octave outside the
   band, 32768 taps (1.5 Hz resolution at 48 kHz). Its complex response is part of every
   prediction.
4. **Delay and polarity.** One setting for every speaker the subwoofer serves: the
   delay change and polarity that maximise the mean level of the predicted sums over
   half to twice each crossover, averaged over the positions and then the speakers.
   The search covers 1.5 periods of the lowest crossover around the lateness the phase
   gives, both polarities; candidates within 0.05 dB of the best are equivalent and the
   one nearest that lateness wins (no needless period of extra delay). Within the
   renderer's 250 ms; when the subwoofer would need less than no delay, every other
   output is delayed instead, which adds latency (named in a warning).
5. **Level.** Per speaker, the subwoofer level change that makes its predicted sum as
   loud from half to 0.7 times the crossover as from 1.4 to twice it (the two half
   octaves where a Linkwitz-Riley pair is equally attenuated); their mean, within
   ±12 dB, is the proposal. Delay and level are found twice in turn.
6. **Report.** Per speaker: its own best setting, each position's best, the summation
   (sum against the parts adding in phase, 0 dB perfect) measured, at its own best and
   with the proposal, the sum's level and balance, and warnings when the shared setting
   costs it more than 1 dB or its positions' best timings spread over a quarter period.

The polarity inversion is carried by the subwoofer's filter (its taps negated, or a
single −1 tap without a mode correction), never by a negative gain.

**Confirmation** measures every group's sum again in the applied state, through the
filters, and compares it per speaker with the prediction at the main seat: RMS
difference over the band, the level and the balance below and above the crossover,
before → predicted → now. A group framed by the subwoofer follows its own delay change.

Everything is saved in the alignment: the plan, captures, analysis, applied state and
confirmation in `alignment-state.json`, each analysis's `bass-integration.json` and the
subwoofer's filter WAV in `bass-integration-<stamp>/`.

## Validation

- Core (`crates/calibration-core/tests/bass.rs`): Linkwitz-Riley parts with known
  lateness, polarity and level. A subwoofer 6 ms late is brought 6.0 ms forward and
  then sums within 0.1 dB of perfect; an inverted one 6 dB loud is inverted back and
  lowered by 6 dB (within 0.3 dB); a filter that already inverts is part of the prediction; a late one
  without delay left delays the other outputs; the phase gives the lateness within 1 ms
  with a 55 Hz mode in the band; positions that disagree by more than a quarter period
  get a compromise nearer the main seat and a warning; speakers with different
  crossovers share one setting; a speaker whose bass goes elsewhere, one whose sum does
  not add up, and one swept too narrowly are left out; either part may be the rest of
  the sum; the confirmation matches a correct application and flags a wrong one. The
  mode correction cuts a 9 dB mode by more than 3 dB, boosts nothing and leaves a dip
  alone.
- Service (`crates/companion-service/tests/bass.rs`): a simulated renderer and room
  (bass-managed sweeps through Linkwitz-Riley crossovers, a subwoofer 17 ms late,
  wired inverted, 6 dB loud, with a 45 Hz mode, filters convolved as the renderer
  does) through measurement at two positions, analysis, Apply, Confirm and Undo: the
  proposal is within 2 ms of the lateness (1 ms off: the subwoofer's own roll-off and
  mode), inverted, the sums within 1 dB of perfect;
  the parts are played in their states and the outputs put back; the confirmation
  matches the prediction within 0.5 dB. A layout whose speakers all stop at 40 Hz
  frames them with the subwoofer and still finds the setting.
- Adapter: bypassed sessions, the `filters_settling` retry, chunked uploads with their
  hash, filter readback, protocol 8 required.

Nothing has been measured on real hardware: no subwoofer measurement, filter or
setting from this step has reached a real renderer or been listened to. The bench's
saved sessions hold no subwoofer or bass-managed take (1 s sweeps over 200 Hz–10 kHz,
no subwoofer row), so there was nothing to analyse offline.

## Limits

- One subwoofer. Several would need one "speakers silenced" state per subwoofer.
- The mains' own correction filters are held off while measuring and are not in the
  prediction; the confirmation plays through them.
- The level target is a flat sum across the crossover; a house curve or the LFE
  channel's level are not considered.
- With the FIR crossover the bass-managed segments carry its latency; both parts carry
  it alike, so their relative timing holds.
