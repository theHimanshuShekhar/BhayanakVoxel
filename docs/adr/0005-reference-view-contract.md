# The reference view's contract

The Reference view is the unbiased view of the world that the Primary view is measured against (`CONTEXT.md`). This ADR is its contract: what it must model, how it estimates light, how it behaves when the view is Still, and how convergence is measured. It was decided in the grilling of 2026-11-12, which closed [#100](https://github.com/theHimanshuShekhar/bhayanakvoxel/issues/100) to [#104](https://github.com/theHimanshuShekhar/bhayanakvoxel/issues/104), against the research in `docs/research/reference-*.md` and the 4090 measurements on [#99](https://github.com/theHimanshuShekhar/bhayanakvoxel/issues/99). It is built by [#187](https://github.com/theHimanshuShekhar/bhayanakvoxel/issues/187) (the bench's convergence phase), [#188](https://github.com/theHimanshuShekhar/bhayanakvoxel/issues/188) (the estimator) and [#189](https://github.com/theHimanshuShekhar/bhayanakvoxel/issues/189) (the still-frame policy), before the lighting tickets.

## Fidelity

**The reference and Primary describe the same world, and the reference adds up the light in that world without bias.** They share the Materials, the sun, the sky model, the emitters, the per-Voxel colours and the loaded scene. A difference between the two views is therefore always a shortcut Primary took (a denoiser, a cache, fewer rays), never a different world. Whether the shared model itself looks right (the atmosphere, the colours) is judged by eye and by the art direction, not by the reference.

We chose this because the reference exists to measure Primary. A reference with its own simpler world (flat colours, a plain sky) differs from Primary everywhere, so the difference says nothing about Primary's lighting. A reference that models more physics than Primary mixes two kinds of error in one number and costs far more to build.

### The bias rule

- An approximation is allowed in the reference only if its **worst-case error is shown to be under 1 % of a pixel's light**, it has a row in the bias ledger below, and a test holds its bound.
- **Clamping, firefly filters, sample rejection, denoising, reprojection and sample reuse are never allowed**, and the reference never reads Primary's light caches.
- Anything else that changes the answer is a bug in the reference.

The 4-bounce cut the reference has today fails the rule: it loses up to 90 % of the light in a snow cave and 29 % under limestone (`docs/research/reference-estimator.md`, section 3.2). It is replaced by the estimator below.

### Lighting features

One row per lighting feature. **Exact** means unbiased. **Approximate, shared model** means both views read the same model and the reference integrates it without bias. **Shared** means both views use the same data and the reference does not judge it. **Absent** means neither view has it.

| Feature | State | Notes |
|---|---|---|
| Sun and shadows | exact | The same disc as Primary's. The known 0.04 % shortfall of the cone sample is divided out, not recorded. |
| Emissive Voxels | exact | Sampled directly as lights, so a room lit by one emissive Voxel converges. |
| Bounce light | exact | Followed until it fades by chance, with no fixed bounce count. The safety cap is a ledger row. |
| Per-Voxel colour variation | exact | One function shared with Primary, a fixed function of the Voxel's position and `materials.toml`. A flat-colour mode remains for tests with a known exact answer. |
| Sky and atmosphere | approximate, shared model | The placeholder gradient today. After [#53](https://github.com/theHimanshuShekhar/bhayanakvoxel/issues/53): the same lookup tables as Primary, a sun-free copy for rays that escape to the sky, and the same haze on the camera ray. The reference does not march through the air. |
| Geometry (LOD and what is loaded) | shared | The reference traces the same loaded scene at the same levels as Primary in that frame. LOD and streaming are judged by the streaming bench and the LOD tests. |
| Glossy reflections | absent | Must enter as exact. |
| Glass and transparency | absent | Water and glass are opaque in both views today. Must enter as exact, transparent shadows included. Light focused through glass is left noisy and is never clamped. |
| Volumetrics (fog, clouds) | absent | May enter as approximate, shared model. |
| Fluids that light | absent | May enter as approximate, shared model. |

### How a feature enters

A PR that adds a lighting feature to Primary must, in the same PR, either add it to the reference or mark it absent in this table with a written reason, and take the views that show it out of the Primary-against-reference gate. A CI test fails when Primary's shader has a lighting feature with no row in this table.

### Bias ledger

| Approximation | Bound | Why it is accepted | Test |
|---|---|---|---|
| Path depth capped at 256 | at most 0.57 % (snow, albedo 0.98); about 0 elsewhere | A path among Materials of albedo 1 would never end. | The closed-box test with a known answer, in CI (#188). |
| No haze on bounce rays (after #53) | to be shown under 1 % by #53, or haze is added to bounce rays | Bounce rays near the ground are short against the scale of the air. | Set by #53. |

## Estimator

- **Sample sequence:** hash-based Owen-scrambled Sobol, computed from whole numbers only (pixel, sample index, seed), for the first 64 dimensions of a path; hashed random numbers for deeper ones. No texture and no blue-noise mask. The same view, sample count and seed give the same sample positions on every machine; the image is bit-identical only on the same binary and driver. The Morton-ordered variant is measured on the 4090 before it is considered.
- **Bounce light:** no fixed bounce count. From the third surface on, a path ends by chance with probability 1 − (the brightest channel of its throughput), and survivors are scaled up to match. This is unbiased. A rock cave costs about a third of what the 4-bounce cut did, and a snow cave about three times as much.
- **Sun:** one sample of the sun's disc and one shadow ray a hit. No extra rays at shadow edges.
- **Emissive Voxels** (with #54): each hit samples one emitter from a bounded list of the emitters near it, built per region when the scene is uploaded, over the solid angle its faces cover, so a close emitter cannot make a bright speckle. Emitters outside the list are found by bounce rays, and the two are weighted (multiple importance sampling) so the sum is unbiased.
- **Sky** (with #53): escaped rays read the sun-free table, sampled in proportion to the table's brightness and weighted against bounce rays in the same way. Until #53 the placeholder gradient is reached by bounce rays only.
- **Nothing is clamped, filtered or rejected.**

## Still frames

- **The Reference view traces at full output resolution**, whatever the Primary view's render scale. The bench reports its cost in ms per megapixel per sample.
- **Still** means the camera, the sun and the window size have not changed for 2 frames and no Edit has happened. The world is frozen while the Reference view accumulates (#94, 2026-11-04).
- **On a change** the screen cuts to the Primary view at once, with no blend, and the Reference view starts again from sample 0. An Edit resets the whole image; there is no partial reset.
- **Each Still frame takes as many samples as fit in 12 ms**, judged from the slowest recent samples of that view, and 1 on the first frame of a new view. The screen keeps presenting at 60 Hz. If even 1 sample does not fit, the frame rate drops and the quality does not.
- **The Reference view fades in over about 150 ms** from its first batch. The fade is for display only and is never stored.
- **No adaptive sampling.** Every pixel gets the same number of samples. Stopping on what the samples show bends the result, which the bias rule forbids without a bound.
- **The target:** FLIP ≤ 0.05 within 0.25 s of stopping, on the 4090 at 1440p, in both bench views. The time to FLIP ≤ 0.02 is reported, not promised.

## Convergence metric

- **The metric is FLIP** (LDR, `flip-rs`, ppd 67.02), the mean over the 8-bit image the player sees. Beside it: linear squared error on the f32 accumulation, and FLIP's 99th percentile as a guard against speckles. The 8-bit image uses the true sRGB curve.
- **The truth** is the same view at 8192 samples with another seed, on the same machine and driver. Two truths compared give the metric's noise floor in that view.
- **The lines:** FLIP ≤ 0.05 is acceptable and ≤ 0.02 is clean. A line counts in a view only where it is at least 4 times that view's floor.
- **The bench** has a convergence phase on two fixed views, the open golden view and a dug pit lit by bounce light, with checkpoints at 1, 4, 16, 64, 256 and 1024 samples. It times single samples. The 8192-sample truth needs a flag; the default is 1024.
- **A regression** is a PR whose curve on the 4090 is worse than main's by more than the floor.
- **CI on lavapipe runs no FLIP.** It keeps the goldens, as a check that the picture has not changed, and has the closed-box test.
- **#55's gate is derived:** the Primary view's FLIP against the truth must be at or under what the raw Reference view scores at 64 samples in the same view on the 4090.

The numbers behind these are in [`docs/measurements.md`](../measurements.md), "The Reference view".

## Considered Options

- **A physical ground truth** (a marched atmosphere, real volumetrics, whatever Primary does): differences then mix Primary's shortcuts with model differences, and the reference becomes a second renderer to build.
- **A deliberately simple lighting check** (flat colours, plain sky, as today): Primary's other features are never compared against anything.
- **A ledger with no fixed bound:** nothing stops a large bias from being accepted under time pressure.
- **No bias at all but rounding:** a path among pure white emitters would never end.
- **The reference first, Primary after:** the yardstick is never behind, but every lighting ticket waits on the slower path.
- **Full detail near the camera in the reference:** needs its own scene, and puts LOD error and lighting error in one number.
- **White noise, or Sobol with a blue-noise mask:** white noise needs several times the samples; a mask makes 1 to 4 samples look better without lowering the error, and needs a texture.
- **Tracing at half resolution:** 4 times cheaper a sample, but about 3 times the samples for the same score, and in the pit its floor is too close to the clean line to measure.
- **Adaptive sampling for display:** 8 to 13 times less work at the 0.05 line, mostly sky, but that line is reached in 6 to 17 samples anyway, and it makes two modes to keep correct.
- **Squared error alone:** shows a wrong estimator, but does not match what people see in dark or busy areas.

## Consequences

- The reference's goldens are remade once, when it takes Primary's colour variation.
- Interiors of bright Materials cost the reference more samples. That cost stays out of Primary.
- #53 owes a sun-free sky table and the bound for haze on bounce rays. [#54](https://github.com/theHimanshuShekhar/bhayanakvoxel/issues/54) owes direct sampling of emissive Voxels in the reference. [#55](https://github.com/theHimanshuShekhar/bhayanakvoxel/issues/55)'s image-difference gate compares views of the same world, and leaves out views that show a feature marked absent.
- The reference says nothing about LOD or streaming, by design.
