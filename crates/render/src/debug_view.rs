//! Debug views: what the kernels draw instead of the lit image, to look at one input of the
//! lighting at a time.

/// What a kernel shows for each pixel. [`Lit`](Self::Lit) is the normal image; the others are the
/// same shading with `Frame.params.z` (see `common.wgsl`) set to the view's [`index`](Self::index).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum DebugView {
    /// The lit image: primary visibility, or the reference path tracer's mean.
    #[default]
    Lit,
    /// The face normal of the first hit, as `(n + 1) / 2` in RGB. The sky is black.
    Normals,
    /// The Material's albedo at the first hit. The sky is black.
    Albedo,
    /// Distance to the first hit, bright when near and dark when far. The sky is black.
    Depth,
    /// How many samples the reference image has accumulated, as a colour that goes from dark
    /// violet to yellow over a thousand samples on a logarithmic scale. In primary mode nothing
    /// accumulates, so it is the colour of one sample.
    SampleCount,
    /// How many frames the upscaler has carried each pixel's history through, from dark violet
    /// (just one: a new camera, or history that the clamp threw away) to yellow (16, the most).
    /// In the reference image it is the sample count.
    HistoryLength,
}

impl DebugView {
    pub const ALL: [DebugView; 6] = [
        Self::Lit,
        Self::Normals,
        Self::Albedo,
        Self::Depth,
        Self::SampleCount,
        Self::HistoryLength,
    ];

    /// The number the shaders switch on.
    pub fn index(self) -> u32 {
        self as u32
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Lit => "Lit",
            Self::Normals => "Normals",
            Self::Albedo => "Albedo",
            Self::Depth => "Depth",
            Self::SampleCount => "Sample count",
            Self::HistoryLength => "History length",
        }
    }

    /// The view after this one, wrapping from the last to [`Lit`](Self::Lit): for a key that
    /// cycles through them.
    pub fn next(self) -> Self {
        Self::ALL[(self as usize + 1) % Self::ALL.len()]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_view_is_the_lit_image_and_the_shaders_see_it_as_zero() {
        assert_eq!(DebugView::default(), DebugView::Lit);
        assert_eq!(DebugView::Lit.index(), 0);
    }

    #[test]
    fn indices_follow_the_order_of_all() {
        for (i, view) in DebugView::ALL.into_iter().enumerate() {
            assert_eq!(view.index() as usize, i);
        }
    }

    #[test]
    fn next_visits_every_view_and_comes_back() {
        let mut view = DebugView::Lit;
        let mut seen = vec![view];
        for _ in 0..DebugView::ALL.len() - 1 {
            view = view.next();
            seen.push(view);
        }
        assert_eq!(seen, DebugView::ALL);
        assert_eq!(view.next(), DebugView::Lit);
    }
}
