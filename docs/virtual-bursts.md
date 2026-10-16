# Signed waveforms and virtual tone bursts

Click a speaker row or analyze a saved take. Results appear in the
resizable panel below the speaker list, with independent scrolling. Acquisition
details and arrival diagnostics remain expandable below the graphs. The source
microphone, position, channel and reference stay attached to the selected result.

**Signed waveform** shows positive and negative values of the full measured
impulse response, before the arrival detector's three frequency windows. The
response still has the sweep's finite bandwidth and deconvolution effects.
**Arrival region** focuses on the existing broad-band candidate, or the largest
absolute impulse sample if no candidate exists. **Overview** spans -50 to +100 ms.
Changing the displayed region does not change the time origin.

**Virtual burst** predicts the response to a causal six-cycle sine packet with a
Hann envelope. Select a center frequency and optionally overlay the source.
Source and prediction share the segment's rounded correlation zero. Source time
zero is a comparison convention, not an absolute renderer emission timestamp.
The traces are each normalized to their own peak, so their heights cannot be used
to compare gain. The underlying report retains the digital amplitudes.

The phase reading is the complex transfer phase at the selected frequency,
relative to that same zero and wrapped modulo 360 degrees. It includes crossover,
room and measurement-chain effects. It is withheld when the nearby spectral
evidence is weak or the exact frequency is near a null. No microphone calibration
is applied. It is not a separate driver's phase, a physical distance or an
inter-speaker alignment value.

No burst is played. These are predictions from the sweep's approximately linear
response, not independent measurements or new evidence of repeatability. An
independent tone-burst acquisition remains a future option. The envelope's rise
time means that its peak is not the propagation delay. Early wiggles may include
pre-ringing; no first-sample or first-cycle rule is used to approve an arrival.

## Bounded offline implementation

The optional `waveforms` field on each impulse segment uses method
`signed-ir-and-virtual-burst-v1`. New takes and offline reanalysis share it.
Original take files and the existing arrival/acquisition verdict are preserved.

Candidate centers are 250, 500, 1000, 2000, 4000, 6000 and 8000 Hz. Only centers
whose burst main lobe fits inside the usable measured band are retained. With
the default 200 Hz–10 kHz sweep, these are 500, 1000, 2000, 4000 and 6000 Hz.
The prediction cannot recover out-of-band burst sidelobes or nonlinear behavior.

Linear FFT convolution uses the full -100 to +150 ms response, tapering its outer
5 ms, and pads to at least the complete convolution length. Each FFT is bounded
to 131,072 bins. Exact-frequency phase uses a complex sum with the common anchor
removed. Its gates reuse the nearest spectral point's 20 dB estimated S/N and
require exact-frequency power above 1% of that point's smoothed power.

Overview and detail traces preserve signed minima/maxima in at most 1600 buckets.
The detail interval is shared by source and prediction, contains source zero,
and follows the same broad-band candidate for every frequency. Analysis runs on
the acquisition/review worker. Painting only reads bounded trace data; selecting
a frequency launches no FFT, thread, audio device or renderer request.

Tests cover signed delays, polarity, phase, gain, several sample rates, a weak
early path followed by a stronger echo, frequency support and invalid inputs.
Saved-take checks verify report publication and unchanged originals. UI checks
cover panel placement, scrolling, graph/frequency selection and no playback.
