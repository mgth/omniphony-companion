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
before starting; unresolved selected speakers block the whole proposal.

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

Verification captures two fresh passes using the saved acquisition settings and
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
submits actions and displays rows in the existing bounded, scrollable result panel.
The two passes keep the shared-reference optimization within each pass. Calibration
and verification do not automatically change output level, repeat forever, or
continue after cancellation.

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
