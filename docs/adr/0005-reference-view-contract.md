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

One row per lighting feature. **Exact** means unbiased. **Approximate, shared model** means both views read the same model and the reference integrates it without bias. **Shared** means both views use the same data and the reference does not judge it. **Primary only** means Primary has a stand-in term that the reference does not have (the reference gets that light from its estimator), so the two differ by design. It is a placeholder, not a permanent state: the feature it stands in for must be removed, or replaced by something the reference matches, before [#55](https://github.com/theHimanshuShekhar/bhayanakvoxel/issues/55)'s gate is built, and the gate leaves out no view for it. **Absent** means neither view has it.

| Feature | State | Notes |
|---|---|---|
| Sun and shadows | exact | The same disc as Primary's. The known 0.04 % shortfall of the cone sample is divided out, not recorded. |
| Emissive Voxels | exact | Sampled directly as lights, so a room lit by one emissive Voxel converges. |
| Bounce light | exact | Followed until it fades by chance, with no fixed bounce count. The safety cap is a ledger row. |
| Per-Voxel colour variation | exact | One function shared with Primary, a fixed function of the Voxel's position and `materials.toml`. A flat-colour mode remains for tests with a known exact answer. |
| Ambient term | primary only | Primary adds a flat 0.25 of the surface's colour to its sun term, a stand-in for the light of the sky and of the bounces. The reference has no such term: it gets that light from the sky lookup and the bounce rays, so the two views differ by design. It is on every lit surface, so no view could be left out of #55's gate for it: [#54](https://github.com/theHimanshuShekhar/bhayanakvoxel/issues/54) (radiance-cache GI), which comes before #55, must remove it or replace it with something the reference matches, before #55's gate is built. |
| Sky and atmosphere | approximate, shared model | The placeholder gradient today. After [#53](https://github.com/theHimanshuShekhar/bhayanakvoxel/issues/53): the same lookup tables as Primary, a sun-free copy for rays that escape to the sky, and the same haze on the camera ray. The reference does not march through the air. |
| Geometry (LOD and what is loaded) | shared | The reference traces the same loaded scene at the same levels as Primary in that frame. LOD and streaming are judged by the streaming bench and the LOD tests. |
| Glossy reflections | absent | Must enter as exact. |
| Glass and transparency | absent | Water and glass are opaque in both views today. Must enter as exact, transparent shadows included. Light focused through glass is left noisy and is never clamped. |
| Volumetrics (fog, clouds) | absent | May enter as approximate, shared model. |
| Fluids that light | absent | May enter as approximate, shared model. |

### How a feature enters

A PR that adds a lighting feature to Primary must, in the same PR, either add it to the reference or mark it in this table with a written reason (absent if neither view has it; primary only for a placeholder that #54 removes), and take the views that show an absent feature out of the Primary-against-reference gate. A CI test fails when a view's shader has a lighting feature with no row in this table.

Features are listed in the shaders (`crates/render/src/shaders.rs` holds the check). A whole-line `// lighting: <Feature>` comment, naming a row as in the first column, must stand on one of the two lines directly above (a comment at the end of a line does not count, and one comment covers the next two lines) each of:

- a read of a source of light: the sun, the sky, a Material's emission, the Voxel's colour (`READS`), with the comment naming that source's feature;
- a call of `trace(`: the camera ray, a bounce, a shadow ray, any other ray, with any row;
- every line of Primary's `shade()` except its control flow, so that a constant term, such as the ambient, has its row;
- every change to `light` in the reference's `radiance()`.

The check reads the whole source of each kernel, `common.wgsl` and `sampling.wgsl` included, not only `primary.wgsl` and `pathtrace.wgsl`. It also fails on a binding it does not know (`BINDINGS`: a radiance cache, a light buffer, whose author then decides whether it carries light) and on a shader file it does not know (`FILES`), on a comment whose feature has no row, and on a feature that Primary names which the reference does not name (in their own files), unless its row says shared or primary only. A feature in the table as absent may not be named by Primary. What it cannot see is a source of light in a function that is neither `shade` nor `radiance`, and a comment that names the wrong row for a `trace(`: review is what is left for those.

### Bias ledger

| Approximation | Bound | Why it is accepted | Test |
|---|---|---|---|
| Path depth capped at 256 | at most ρ^256 of the light in a sealed cavity, where ρ is the brightest channel of the Material's base colour (the colour function clamps every channel to 1 after its variation, so ρ ≤ 1): 0.57 % at snow's 0.98, 0.00 % at 0.9. Varied snow is not exactly that: its value jitter of 0.04 takes the brightest channel to 1.0 on about a quarter of its patches, and the closed-box test is in flat colours; the sentence on ρ = 1 below applies to those patches. **For ρ = 1 it does not hold:** a Material with a channel at exactly 1.0 (the light Materials) gives ρ^256 = 1, so a cavity of such surfaces alone, with no opening, has no bound under 1 % (its true answer has no limit, and the cap gives 256 bounces of it). In an open scene a surface of ρ = 1 is bounded by the light that leaves: if a fraction e of bounce rays escapes at each bounce, the loss is at most (1 − e)^256, under 1 % when e is at least 1.8 %. | A path among Materials of albedo 1 would never end. | The closed-box test with a known answer, in CI (#188): `crates/render/tests/estimator.rs`, at albedo 0.9 and 0.98 to five standard errors of 0.46 %, and a depth cap of 190 put in fails it. ρ = 1 is not tested: it has no answer to compare with. |
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
- #53 owes a sun-free sky table and the bound for haze on bounce rays. [#54](https://github.com/theHimanshuShekhar/bhayanakvoxel/issues/54) owes direct sampling of emissive Voxels in the reference. [#55](https://github.com/theHimanshuShekhar/bhayanakvoxel/issues/55)'s image-difference gate compares views of the same world, and leaves out views that show a feature marked absent. It leaves out none for the ambient term, which #54 removes or matches before the gate is built.
- The reference says nothing about LOD or streaming, by design.
