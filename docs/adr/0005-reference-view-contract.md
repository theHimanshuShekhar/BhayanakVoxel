# The reference view's contract

**Status: draft.** The fidelity contract below was decided on 2026-11-12 ([#103](https://github.com/theHimanshuShekhar/bhayanakvoxel/issues/103)). The sample sequence and estimator ([#100](https://github.com/theHimanshuShekhar/bhayanakvoxel/issues/100)), the still-frame scheduling ([#101](https://github.com/theHimanshuShekhar/bhayanakvoxel/issues/101)) and the convergence metric ([#102](https://github.com/theHimanshuShekhar/bhayanakvoxel/issues/102)) are still open; [#104](https://github.com/theHimanshuShekhar/bhayanakvoxel/issues/104) finishes this ADR when they are decided.

## Fidelity

**The reference and Primary describe the same world, and the reference adds up the light in that world without bias.** They share the Materials, the sun, the sky model, the emitters, the per-Voxel colours and the loaded scene. A difference between the two views is therefore always a shortcut Primary took (a denoiser, a cache, fewer rays), never a different world. Whether the shared model itself looks right (the atmosphere, the colours) is judged by eye and by the art direction, not by the reference.

We chose this because the reference exists to measure Primary. A reference with its own simpler world (flat colours, a plain sky) differs from Primary everywhere, so the difference says nothing about Primary's lighting. A reference that models more physics than Primary mixes two kinds of error in one number and costs far more to build.

### The bias rule

- An approximation is allowed in the reference only if its **worst-case error is shown to be under 1 % of a pixel's light**, it has a row in the bias ledger below, and a test holds its bound.
- **Clamping, firefly filters, sample rejection, denoising, reprojection and sample reuse are never allowed**, and the reference never reads Primary's light caches.
- Anything else that changes the answer is a bug in the reference.

The 4-bounce cut the reference has today fails the rule: it loses up to 90 % of the light in a snow cave and 29 % under limestone (`docs/research/reference-estimator.md`, section 3.2). It is replaced; #100 decides how.

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
| Path depth capped at 256 | at most 0.57 % (snow, albedo 0.98); about 0 elsewhere | A path among Materials of albedo 1 would never end. | The closed-box test with a known answer, in CI (built with #100's estimator). |
| No haze on bounce rays (after #53) | to be shown under 1 % by #53, or haze is added to bounce rays | Bounce rays near the ground are short against the scale of the air. | Set by #53. |

## Considered Options

- **A physical ground truth** (a marched atmosphere, real volumetrics, whatever Primary does): differences then mix Primary's shortcuts with model differences, and the reference becomes a second renderer to build.
- **A deliberately simple lighting check** (flat colours, plain sky, as today): Primary's other features are never compared against anything.
- **A ledger with no fixed bound:** nothing stops a large bias from being accepted under time pressure.
- **No bias at all but rounding:** a path among pure white emitters would never end.
- **The reference first, Primary after:** the yardstick is never behind, but every lighting ticket waits on the slower path.
- **Full detail near the camera in the reference:** needs its own scene, and puts LOD error and lighting error in one number.

## Consequences

- The reference's goldens are remade once, when it takes Primary's colour variation.
- Interiors of bright Materials cost the reference more samples. That cost stays out of Primary.
- #53 owes a sun-free sky table and the bound for haze on bounce rays. [#54](https://github.com/theHimanshuShekhar/bhayanakvoxel/issues/54) owes direct sampling of emissive Voxels in the reference. [#55](https://github.com/theHimanshuShekhar/bhayanakvoxel/issues/55)'s image-difference gate compares views of the same world, and leaves out views that show a feature marked absent.
- The reference says nothing about LOD or streaming, by design.
