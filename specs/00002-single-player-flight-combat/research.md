# Research: Single-player Flight & Combat Slice (E002)

Product research informing story priorities, acceptance criteria, and edge cases for the first-playable single-player vertical slice. Not implementation guidance.

## Momentum flight feel & flight-assist toggles

Recommendation: default Flight-Assist ON — a fly-by-wire layer over true Newtonian motion that dampens unwanted drift, caps velocity toward the aim vector, and makes the ship "fly where you point" for accessibility. Offer Assist OFF as a decoupled mode that preserves full momentum (face one way, drift another), cancelled only by explicit opposing thrust. Good-feel signals to assert: responsive rotation, readable/intentional drift, deliberate deceleration, no uncommanded skidding. Avoid: decoupled-only design, un-tunable drift, instant velocity changes that erase inertial weight. Sources: Elite Dangerous flight model wiki; Elite PvE flight-assist discussion.

## Render / simulation decoupling

Recommendation: drive a fixed-timestep `sim` and interpolate rendering between the two latest physics states (alpha = accumulator/dt). This keeps motion smooth and frame-rate independent and supports a 60+ FPS feel gate; flight should feel identical at 30/60/144 FPS. Avoid: stepping physics with a variable frame delta (non-deterministic, breaks the integrator↔analytic invariant from E001), rendering raw physics state (stutter), and unbounded catch-up (spiral of death). Source: Fix Your Timestep (Gaffer on Games).

## Fast-projectile collision (swept / CCD)

Recommendation: use swept/continuous tests resolved at time-of-impact — a fast projectile can sit in front of a target one frame and behind it the next, tunneling with no hit. The slice MUST test high relative closing velocity, grazing/tangent hits, small/thin targets, and simultaneous multi-hit frames; acceptance is "no missed hits across the full velocity range." Avoid: discrete point-overlap for fast projectiles, and CCD alone on ultra-thin colliders (pair it with velocity caps and slightly thicker proxy hitboxes). Sources: Pulsegeek continuous-vs-discrete collision; Adam Heins CCD visual.

## Vertical-slice / first-playable acceptance

Recommendation: make "feels good" testable by pinning it to a minimal demonstrable loop — pilot → aim → fire → hit → destroy — completable end-to-end in both assist modes, with fun surviving real constraints (frame budget, input latency). Keep P1 to that loop alone so it is independently testable. Avoid scope traps: placeholder sprawl, polishing visuals before the loop is proven, hero-mode tuning that masks bad feel, and adding networking or extra ship systems before feel is validated. Sources: Tono vertical-slice guide; Rami Ismail prototypes-and-vertical-slice.
