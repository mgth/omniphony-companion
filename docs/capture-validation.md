# Capture validation

The initial acquisition workflow supports one selected input on an audio interface.
Software tests pass synthetic data through the estimator; they are not evidence
of microphone or installation performance.

## First hardware session

1. Build the matching Omniphony calibration PR and companion commit. Confirm
   output routing, microphone input channel and fixed interface gain.
2. Disable microphone processing, automatic gain and monitoring that could feed
   the microphone back into the speakers. Discover the renderer and list inputs.
3. Take a reference-on-reference measurement at a conservative level. Inspect
   the WAV and report. Compare at least five takes without moving anything.
4. Measure a second full-range speaker using the same fixed reference output.
   Repeat, then introduce a known renderer output delay and rediscover. Verify
   that the change appears in the relative peak diagnostic.
5. Repeat with input/output at different supported rates. Inspect drift and
   residual variation. Host timestamp flags must be investigated, not ignored.
6. Exercise Ctrl+C, device disconnection, input starvation, renderer loss and
   configuration changes. Check bounded playback termination, rejected-take
   metadata and retained partial audio.
7. Check clipping and low-SNR rejection at the input; record the actual accepted
   operating range. Keep raw takes outside Git.

Record interface/driver/host configuration with the local session, repeated
relative delays and reference levels, drift distribution, failure reasons and
whether the direct arrival is distinguishable from reflections. Do not infer
accuracy from a single take.

## Pending work before automatic correction

- Validate stable capture timestamps and discontinuity detection on real ALSA,
  CoreAudio and WASAPI interfaces. Hidden OS resampling is not currently detected.
- Quantify repeatability, nonlinear clock behavior and confidence limits.
- Derive impulse responses and identify direct arrivals; compare level estimates
  over a defined band/window with microphone calibration actually applied.
- Consider an electrical loopback reference for suitable multichannel interfaces.
- Convert accepted measurements into validated observation sessions with
  persistent installation identity, then implement apply/readback/remeasure.
