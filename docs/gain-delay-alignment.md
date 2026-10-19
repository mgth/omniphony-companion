# Repeated delay and level alignment

This experimental workflow completes the software path from microphone captures
to explicit live gain/delay application, repeated acoustic verification and guarded
restoration. It does not establish hardware accuracy or prove that an early arrival
is the unobstructed direct path. Mission 1 still requires installation validation.

## Evidence and scope

A fixed microphone and reference speaker are used for two complete independent
passes. Each pass preserves the existing reference–target–reference sequence and
its raw captures. Selected speakers must support broadband timing and the common
500–2000 Hz level band. Muted, unwired or incompatible speakers should be excluded
before starting. Unresolved selected speakers remain in the report with an explicit
exclusion reason. The proposal uses the qualified subset when at least two speakers
qualify; excluded gains and delays are unchanged. Shared-reference inconsistency,
cancellation or session-wide acquisition failure still blocks the proposal.

The level estimate uses the unnormalized, regularized transfer response, sampled
on the same 25-point logarithmic grid for every segment. Magnitude in dB is
interpolated in log frequency; powers are averaged with trapezoidal endpoint
weights. Level qualification requires at least 20 dB integrated estimated SNR
over this complete band for the target and both references. Weak individual
frequencies remain in the estimate with unchanged weights and a visible warning.
Acquisition qualification separately requires at least 15 dB SNR over the excited
sweep band; the former broadband 20 dB cutoff no longer hides results. The response
includes room energy within the existing impulse window. A microphone amplitude file is preserved but not applied;
the method ID explicitly identifies this uncalibrated relative weighting. It is
not sweep RMS, absolute SPL, or an isolated direct-sound level.

Arrival estimates retain the existing band/threshold/reference waveform gates.
Independent target estimates must agree within 0.15 ms and levels within 0.50 dB.
Reference levels must agree within 0.50 dB within and across takes. These gates are
prototype sensitivity/repeatability tests, not statistical accuracy bounds. A
reproducible reflection can still pass: no algorithm here proves direct-path identity.

The pure core qualifies repeated estimates and proposes settings. Relative times
may be signed, and transfer gains may exceed 0 dB. The legacy observation field
`level_dbfs` stores relative transfer dB for this method; its method identifier,
not that historical name, defines its meaning. Original acquisition reports retain
`valid_for_correction: false`; only the new repeated session supplies measured
observations. Simulated/imported sessions cannot enter this live workflow.

## Application and verification

Targets are absolute: add only the new timing difference to existing delay and
attenuate louder speakers to the quietest selected speaker. The service retains
both the full renderer snapshot and the selected-output core profile. Other
outputs remain unchanged. Core policy bounds and renderer limits are both checked.

Protocol 4 `set_outputs` checks the revision while serializing live-state writers,
validates the entire request, publishes all selected gains/delays together and
returns a new snapshot. The adapter checks all reported physical settings, including
unchanged outputs. No save-config command is sent. Do not reconfigure topology or
host audio during calibration; any changed revision blocks the next operation.

Before sending a mutation, the service persists an uncertain state. A missing or
incorrect acknowledgement cannot be presented as success or automatically retried.
A confirmed state is saved atomically. Reapplying/restoring is idempotent, and
restoration refuses intervening edits rather than overwriting them.

Verification captures two fresh passes of the fixed corrected subset using the saved acquisition settings and
applied snapshot. It compares measured spread with the baseline at the same
position, using prototype 0.10 ms / 0.50 dB tolerances. Predicted correction alone
cannot pass verification. Incomplete or ambiguous verification clears any previous
success and retains its recordings. Restoration is explicit; failed verification
does not silently change settings again.

## Persistence and frontend

`alignment-plan.json` is immutable. `alignment-state.json` is atomically replaced
as application and verification progress, retaining the original plan and last
confirmed renderer revision. Pass and verification directories contain recordings
and original take reports. Loading a session is read-only with respect to hardware;
no measurement or mutation occurs on startup or load. A restarted renderer has a
new identity/revision and invalidates the saved application state.

Workers own acquisition, analysis, persistence and network requests. The UI only
submits actions and draws the result: an alignment chart (Timing lane: arrival
relative to the reference, repeat spread, target and added delay; Level lane:
level relative to the quietest reliable speaker, corrected level and gain), then
speaker cards with the measured values and planned change, then one action bar
(Apply to Omniphony, Original/Corrected, Confirm by re-measuring, Undo). Raw
per-speaker numbers, exclusion reasons, pass details and the Reanalyze/Update
actions are shown with **Lab** on. The lower panel is reserved for the selected
speaker's individual measurement and graphs; there is no separate Alignment tab.
The two passes keep the shared-reference optimization within each pass. Calibration
and verification do not automatically change output level, repeat forever, or
continue after cancellation.

**Measure again into the session** (a card's Measure choice) captures one speaker
twice more, one take per pass in the session's reference–speaker–reference sequence,
under the session's settings and the renderer state it expects (its original
snapshot, or the restored one after a retake). The two takes replace that speaker's
repeats in `alignment-state.json`; the other rows, the pass reports and the plan are
unchanged, the proposal is rebuilt and validated as **Update correction proposal**
does, and the new takes live in `pass-1-again-*` / `pass-2-again-*` directories of the
session. A clipped take is measured again at a lower gain as in a full alignment. The
action is refused, and its card choice greyed with the reason, for applied or
uncertain sessions (undo first), changed settings, another renderer or a renderer
whose settings differ; a stopped or failed re-measure keeps the previous takes.

**Reanalyze saved passes** recomputes the two passes from retained WAV files and
shows new estimates, repeatability and quality grades. It writes a separate
`alignment-review-*` directory, preserves every original report and cannot apply
a profile. Applied or uncertain sessions retain their restoration context and
cannot be replaced by this action. See [measurement quality](measurement-quality.md).

## Validation

Numerical tests check known gain, polarity, delay and sample-rate changes; weak
spectral evidence; signed relative times; repeat instability; and invalid values.
A synthetic acoustic rig exercises capture analysis, proposal, live-state simulation,
fresh verification, file reload, restoration, stale-state rejection and lost replies.
Protocol tests exercise actual local TCP framing and the real renderer service.
These tests do not substitute for a microphone/interface bench test.

Installation check: repeat a fixed-position measurement; inspect arrival candidates
and band evidence; apply only a qualified proposal; run verification; compare the
new measured spreads and test restoration. Keep levels comfortable. Increasing
volume is not a remedy for ambiguous arrival timing. Longer bass windows,
microphone calibration, multipoint EQ and subwoofer integration remain deferred.


## Partial proposals and existing sessions

The session summary reports how many speakers have targets and how many are
excluded. Each excluded speaker shows a short reason in the chart and its card
(the technical reason is in the tooltip and in Lab's exclusion list). Excluded observations do not determine the latest arrival, quietest
level, or shared-reference consistency of the eligible subset. The two-capture,
SNR, clipping and repeatability thresholds are unchanged. At least two speakers
must qualify, even when the timing-reference speaker itself has no correction
target; remaining captures must still contain valid reference bursts.

Applying and restoring send only the included outputs. Full renderer readback
still checks that every other output is unchanged. Verification keeps this exact
membership and requires new evidence for every corrected output; it cannot claim
success for a smaller surviving subset.

For complete original saved sessions from the former all-or-nothing workflow,
**Update correction proposal** revalidates the saved independent estimates and
builds eligible targets without playback or renderer writes. It updates only
`alignment-state.json`; the original plan, captures and rejection reasons remain
available. Applied/uncertain profiles, physical-volume changes, incomplete passes,
simulated data and diagnostic-only reanalyses cannot be promoted by this action.

## Reason codes

Exclusions and blockers are typed in `calibration-core` (`calibration_core::reason`)
and produced where they are found: the core's repeatability gates, the service's
per-take estimate and the subset policy. Each `ExclusionReason` (no signal, missing
timing references, clipped, low SNR with its band, value and interval, unstable reference,
repeat spread in ms/dB, no clear arrival, not measured twice, cancelled, capture
failed, session mismatch, far arrival, other) and each `Blocker` has one technical
`Display`, shown in Lab, and one short `human()` sentence, shown in the chart and
cards, with a severity (red: nothing captured; amber: not trusted; blue: set aside).
The UI never interprets text.

Saved sessions are additive: rows gain `reason_code` and `include_anyway`, repeats
gain `codes` and the session gains `blocker_codes`. The former text fields are still
written (as the codes' `Display`), so older builds keep reading new sessions. Sessions
saved before the codes load unchanged; their text is classified once
(`ExclusionReason::from_message`, also used for analysis messages that are still
text), and older text blockers are kept verbatim as `Other`, which still block an
update. Evidence validation compares membership and estimates, not the wording of
an exclusion.

## Far arrivals are set aside

A speaker can be perfectly repeatable and still arrive far from the others, typically
because it plays through another output device or a processing path with its own
latency (the session of 16 Oct 2026 had two speakers at +177 ms). The former policy
delayed every qualified speaker to the latest arrival, which would have delayed the
whole room by 177 ms to match two speakers.

The proposal policy now sets aside, by default, every repeatable speaker whose arrival
is more than **20 ms** from the median of the repeatable speakers. For an even count
the median is the middle arrival nearer the timing reference (0 ms), not the mean of
the two, so a room split into two equal groups keeps the reference's group instead of
setting everyone aside. Set-aside speakers keep their settings and their estimates,
are shown in blue with the offset ("Set aside: 177 ms later than the others."), and
do not influence the latest arrival, quietest level, reference-level consistency or
minimum-count check of the corrected subset.

The trade-off is explicit. Setting a late device aside keeps the rest of the room
tight and avoids adding a large common delay, which would shift audio against video
(lip sync) and use the renderer's limited delay capacity; but the set-aside speaker
then stays early or late relative to the others until its route latency is fixed
outside the Companion. Aligning to it makes the whole room coherent at the cost of
that common delay. The user chooses per speaker with **Align to … anyway** below the
chart (and **Set … aside again**). The flag is saved in the session
(`include_anyway`); the proposal is rebuilt from the saved passes without playback,
under the same rules as **Update correction proposal**, and the renderer's maximum
delay still applies. A session saved before this policy whose profile already
corrects a far speaker loads with that speaker included, so it stays valid and
restorable.


## Low signal/noise names the sweep that fell short

Each take plays three sweeps: the reference speaker, the speaker, the reference
again. The acquisition (15 dB, excited band) and level (20 dB, 500–2000 Hz) floors
apply to the weakest of the three, and the exclusion names it: "Reference sweep
after it was too noisy. Measure again." for a disturbed reference, "Too noisy to
trust." when the speaker's own sweep fell short. The technical text adds "in the
reference sweep after the speaker" (or before, or "in the speaker's own sweep");
sessions saved before this carry no interval and read as before.

## Clipped or disturbed speakers are measured again

A speaker that clips in its own interval of a pass is measured again in the same pass,
with its output gain lowered by the estimated overdrive plus 3 dB, rounded up (for
example 13 dB for a take 9.3 dB over full scale). **Measurement settings → Re-measure
clipped speakers at a lower gain** turns this off; it is on by default. The gain goes
through the same live output settings as **Apply**, never `config.yaml`, and is
restored right after the retake, before the next pass. The retake replaces the
clipped take of that pass, and its estimate adds the lowered gain back to the level
(and to any remaining overdrive), so both passes and the proposal read the speaker
at its own setting; `output_gain_offset_db` on each repeat records it.

Lowering one output cannot help when the reference intervals clip too (the
reference speaker itself, or a capture too hot overall), or when the gain would go
below the renderer's -60 dB: those speakers keep their clipped takes and the hint to
lower the microphone gain or test level. A retake that fails to capture also keeps
the clipped take. If a gain cannot be restored, the alignment is blocked with the
speakers to fix.

A speaker excluded only because a reference sweep of its take fell below a floor is
measured again in the same retake batch, at its own gain, when another take of the
pass has clean reference sweeps: the reference passes, so the shortfall was a passing
disturbance in the room. This does not depend on the option above and never changes
the renderer. When every take's reference falls short, the reference itself is too
weak or the room too noisy, and nothing is measured again. A completed retake replaces
the first take, failed or not.

Restoring gives the renderer a newer revision of the original configuration. The
alignment keeps it as `restored_snapshot`, so **Apply** compares against the live
state, and lists every lowered and restored state in `retake_snapshots`, so
reanalysis accepts takes from those states only and recovers each retake's offset
from its own snapshot. Verification never lowers a gain: it measures the applied
state as it is, and only measures a disturbed reference again.
