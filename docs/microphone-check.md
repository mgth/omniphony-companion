# Microphone preparation and capture feedback

1. Choose the capture source, interface input channel and sample rate. Port names
   describe input channels, not the speaker being measured.
2. Click **Check microphone**. The meter runs for ten seconds, plays no sound and
   saves no audio. Speak near the microphone and confirm the meter reacts. Use
   **Stop check** to finish early. Input controls and playback stay locked until
   the worker releases the capture device.
3. Check only the connected speakers. The list summarizes manually excluded and
   unavailable outputs; checkbox preferences persist across restarts. The fixed
   reference speaker must also be connected, even if excluded as a target.
4. Choose **Measure selected speakers** for one diagnostic pass, or **Align levels
   and delays** for two independent passes and a proposal. Only **Apply gains and
   delays** changes output settings. Keep microphone position and input gain fixed.

The meter distinguishes no frames, exact digital silence, activity, clipping and
capture faults. Its last reading is labeled as stopped, never as a lasting readiness
certificate. Changing the source, channel or sample rate invalidates that reading.
The deadline and cancellation live in the service worker and do not depend on UI
repainting. Samples use a bounded in-memory capture; no callbacks allocate for the meter.

Every acquisition (including alignment and verification passes and CLI capture)
uses its own stream for a 600 ms metered quiet lead-in. It requires enough new frames
and checks the most recent third of a second before requesting playback. Silence
means every sample is exactly zero, not simply a quiet room. Low background noise
is allowed, and the existing sweep SNR/arrival/repeatability gates remain unchanged.
Known digital sink monitors may be silent before playback; their monitor provenance
and exclusion from acoustic alignment remain unchanged. Input loss after this check
still relies on capture faults and subsequent acoustic analysis; this is not a
continuous guarantee of microphone health.

An input failure retains the partial recording and reports the actual input problem.
All-zero saved takes and batches also receive the explicit no-signal diagnosis rather
than a reference-search error. Source recordings and prior verdicts are not rewritten.

New alignment sessions retain baseline and verification pass summaries separately.
They distinguish a stopped pass from one never started. "Complete" means per-speaker
analysis ran, not that every arrival is qualified. Per-speaker reasons still block
proposals. Old sessions without this history show that the status was not recorded.
