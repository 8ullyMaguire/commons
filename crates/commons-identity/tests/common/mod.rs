//! Fixtures shared by the identity acceptance tests.
//!
//! Extracted rather than duplicated when T-P3-003 needed the same vector
//! geometry. Two copies of a seeded generator drift: one gets a fix the other
//! does not, and the two then disagree about what a distance of 0.4 means. A
//! shared module cannot drift that way, and it makes the intent explicit --
//! these are the *engine's* fixtures, not each test's own.
//!
//! Everything here is deterministic. A clustering test with an unseeded
//! generator fails intermittently, and an intermittently failing test gets
//! deleted rather than fixed.

#![allow(dead_code)] // Each test binary uses a subset.

// Only what this module's own code uses. A test binary that needs more imports
// it directly: re-exporting a name "just in case" costs a compile error under
// `-D warnings` in the binary that does not use it, which is the wrong way round
// -- the fixture module should be the narrow one and the callers explicit.
pub use commons_identity::cluster::{self, Engine, EngineConfig};
pub use commons_ml::face::Embedder;
pub use commons_store::{db, Store};

/// The fixture's dimensionality: the real embedding width.
pub const D: usize = Embedder::ARCFACE_WIDTH;
/// One of the ten people.
pub const PEOPLE: usize = 10;
/// The UI's "worth showing" bar (§7.1). A cluster below it is still stored and
/// still a real cluster; it is just not surfaced on its own.
pub const BROWSABLE_MIN: i64 = 3;
/// Appearances per person.
pub const PER_PERSON: usize = 20;

/// SplitMix64: a small, seeded, well-distributed generator.
///
/// Seeded because a clustering test with an unseeded generator fails
/// intermittently, and an intermittently failing test gets commented out instead
/// of fixed.
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Rng(seed)
    }

    /// A uniform u64.
    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// A standard normal, via Box-Muller.
    ///
    /// `ln` of a uniform can be zero, so the argument is clamped: a Gaussian
    /// generator that returns infinity makes every distance NaN and the test
    /// fails with a message that points nowhere.
    pub fn normal(&mut self) -> f64 {
        let u1 = ((self.next_u64() >> 11) as f64 + 1.0) / ((1u64 << 53) as f64 + 1.0);
        let u2 = ((self.next_u64() >> 11) as f64) / ((1u64 << 53) as f64);
        (-2.0 * u1.ln()).sqrt() * (std::f64::consts::TAU * u2).cos()
    }

    /// A unit vector, or near enough that the caller does not have to care.
    pub fn unit(&mut self) -> Vec<f32> {
        let v: Vec<f32> = (0..D).map(|_| self.normal() as f32).collect();
        Embedder::l2_normalize(&v).to_vec()
    }
}

/// The whole fixture: vectors, the ten ground-truth people, the weight-change
/// indices, and the planted lookalike pair.
pub struct Fixture {
    /// One entry per appearance, in a fixed order.
    pub vectors: Vec<Vec<f32>>,
    /// The ground-truth person for each appearance.
    pub truth: Vec<usize>,
    /// Per person, which appearance indices are the weight-change cases.
    pub weight_change: Vec<Vec<usize>>,
    /// The two appearance indices that are the planted lookalike pair.
    pub lookalike: (usize, usize),
}

impl Fixture {
    /// Build the fixture. Deterministic for a given `seed`.
    pub fn build(seed: u64) -> Self {
        let mut rng = Rng::new(seed);
        let people: Vec<Vec<f32>> = (0..PEOPLE).map(|_| rng.unit()).collect();

        let mut vectors = Vec::new();
        let mut truth = Vec::new();
        let mut weight_change = Vec::new();

        for (p, centre) in people.iter().enumerate() {
            let mut heavy = Vec::new();
            for i in 0..PER_PERSON {
                // Three per person are the weight-change cases. The direction is
                // fixed per person so the same three are always heavy, and a
                // failure is reproducible.
                //
                // The heavy sigma is small because the noise is added to each of
                // D components independently, so the perturbation of the *unit
                // vector* grows as sigma * sqrt(D), not as sigma. At D = 512 a
                // naive 0.11 lands at a distance of about 0.62 from the person's
                // centre -- past the engine's 0.55 threshold, which would make
                // the "weight change" indistinguishable from a different person
                // and the test would be asserting a distance the engine is
                // correct to refuse. The value below puts the heavy cases at
                // roughly 0.30: several times the light spread of 0.09, and far
                // inside a threshold that still separates two people at ~1.0.
                let is_heavy = i < 3;
                let sigma = if is_heavy { 0.030 } else { 0.018 };
                let noisy: Vec<f32> = (0..D)
                    .map(|k| centre[k] + (sigma * rng.normal()) as f32)
                    .collect();
                let v = Embedder::l2_normalize(&noisy).to_vec();
                if is_heavy {
                    heavy.push(vectors.len());
                }
                vectors.push(v);
                truth.push(p);
            }
            weight_change.push(heavy);
        }

        // The planted lookalike.
        //
        // What is ambiguous is a vector equidistant from two *different* people,
        // which no single nearest-neighbour rule can resolve: a rule that picks
        // the lower cluster id silently asserts it is one of them.
        //
        // The subtle part is *what* it has to be between. The engine compares
        // against cluster centroids, and a centroid is the mean of the vectors
        // that joined -- not the synthetic centre those vectors were generated
        // from. The two differ, because the heavy weight-change cases pull the
        // mean away from the centre. So the midpoint is taken of the two
        // people's *actual generated vectors*, which is the best available
        // estimate of what the centroids will be, and the test then measures
        // the real distances and refuses to pass if the fixture did not land
        // where it claimed. A fixture that is only approximately ambiguous
        // would turn a real engine bug into an intermittent test failure, which
        // is the one outcome a seeded generator is supposed to prevent.
        let mean_of = |p: usize| -> Vec<f32> {
            let lo = p * PER_PERSON;
            let mut m = vec![0f32; D];
            for v in &vectors[lo..lo + PER_PERSON] {
                for (acc, x) in m.iter_mut().zip(v) {
                    *acc += *x;
                }
            }
            for x in m.iter_mut() {
                *x /= PER_PERSON as f32;
            }
            Embedder::l2_normalize(&m).to_vec()
        };
        let midpoint: Vec<f32> = (0..D)
            .map(|k| (mean_of(0)[k] + mean_of(1)[k]) / 2.0)
            .collect();
        let mut jitter: Vec<f32> = (0..D).map(|_| (0.002 * rng.normal()) as f32).collect();
        for k in 0..D {
            jitter[k] += midpoint[k];
        }
        let look = Embedder::l2_normalize(&jitter).to_vec();

        // Two appearances with the same embedding. If the engine has an
        // "already seen this vector" path that does not consider ambiguity,
        // these two take the same decision and one of them is wrong.
        let lookalike = (vectors.len(), vectors.len() + 1);
        vectors.push(look.clone());
        truth.push(usize::MAX); // not either person
        vectors.push(look);
        truth.push(usize::MAX);

        Fixture {
            vectors,
            truth,
            weight_change,
            lookalike,
        }
    }
}

/// An engine over an empty library.
pub async fn engine() -> (tempfile::TempDir, Store, Engine) {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open_library(dir.path()).await.unwrap();
    let engine = Engine::open(&store, EngineConfig::default()).await.unwrap();
    (dir, store, engine)
}

/// The object row an appearance hangs off. `appearance.object_id` is a foreign
/// key, so every id the fixture invents has to exist before it can be attached
/// to anything -- and the tests are not exempt from the constraint they are
/// testing against.
pub async fn object(store: &Store, id: &str) {
    db::insert_object(store, id, "photo").await.unwrap();
}

/// The fixture index an object name refers to: `obj17` is appearance 17.
pub fn index_of(object_id: &str) -> usize {
    object_id
        .strip_prefix("obj")
        .and_then(|n| n.parse().ok())
        .unwrap_or_else(|| panic!("{object_id} is not a fixture object name"))
}

/// An orthogonal coordinate system at `origin`.
///
/// Two vectors built at distances x and y from the origin along the *same* `d`
/// are then at `|x - y|` from each other, which is what makes an A~B, B~C, not
/// A~C chain expressible at all. Building each one against a freshly derived
/// direction would give three unrelated vectors and the arithmetic in the
/// assertions would be about nothing.
pub struct Frame {
    pub origin: Vec<f32>,
    pub d: Vec<f32>,
}

impl Frame {
    pub fn new(origin: &[f32]) -> Self {
        // Gram-Schmidt against a basis vector. `d` must be orthogonal to
        // `origin` in *every* component, not just the two that were written --
        // zeroing i and j leaves the other 510 to contribute, and a `d` that is
        // not orthogonal makes "at distance x from the origin, in direction d"
        // mean something that depends on x, which is the opposite of what this
        // helper is for.
        let n = origin.len();
        let i = (0..n)
            .find(|k| origin[*k].abs() > 0.5 / (n as f32).sqrt())
            .unwrap_or_else(|| panic!("every component is ~0, so this is not a vector"));
        let mut d = vec![0f32; n];
        d[i] = 1.0;
        let dot: f32 = d.iter().zip(origin).map(|(x, y)| x * y).sum();
        for (x, y) in d.iter_mut().zip(origin) {
            *x -= dot * y;
        }
        let norm: f32 = d.iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!(
            norm > 1e-6,
            "e_{i} is parallel to the origin vector, so there is no orthogonal \
             direction to build in"
        );
        Frame {
            origin: origin.to_vec(),
            d: d.iter().map(|x| x / norm).collect(),
        }
    }

    /// A vector at `distance` from the origin, perturbed.
    ///
    /// The perturbation is not decoration. It is 0.004 per component over 512
    /// dimensions, which is a *vector* error of about 0.09 and a distance error
    /// of roughly 0.36 -- larger than every threshold a test is likely to
    /// choose. Tests that need a known distance use [`Frame::exactly`], and this
    /// exists for the tests that need "a different vector near here" rather
    /// than "a vector at exactly this distance".
    pub fn at(&self, distance: f32, away: &mut Rng, sign: f32) -> Vec<f32> {
        at_distance_in(self, distance, away, sign, true)
    }

    /// A vector at *exactly* `distance` from the origin.
    ///
    /// `at` cannot do this: its per-component noise is worth about 0.36 of
    /// distance, which silently swamps a 0.60 threshold and makes a fixture
    /// that reads as "0.70 away" arrive as 0.45.
    ///
    /// **These do not add along the arc, and a test that assumes they do will
    /// get a number it did not ask for.** Two vectors at `x` and `y` from the
    /// origin are `1 - cos(acos(1-x) - acos(1-y))` apart, not `|x - y|`: 0.05
    /// and 0.70 are 0.417 apart, not 0.65. Use [`Frame::from_pair`] when what
    /// matters is the distance *between* two vectors.
    pub fn exactly(&self, distance: f32, sign: f32) -> Vec<f32> {
        at_distance_in(self, distance, &mut Rng(0), sign, false)
    }

    /// A second unit vector at exactly `distance` from `first`, with no noise.
    ///
    /// The pair a test usually wants to state -- "these two are 0.70 apart" --
    /// and the one [`Frame::at`] cannot express. `first` must itself be a unit
    /// vector; anything else makes the answer wrong in a way the test cannot
    /// see. `second` is `first` rotated within the plane `first` and a fresh
    /// orthogonal direction span, by the angle whose cosine distance is
    /// `distance`, so the result is at `distance` from `first` by construction
    /// rather than by an accident of where two arc positions happen to fall.
    pub fn from_pair(first: &[f32], distance: f32) -> Vec<f32> {
        let orth = Frame::new(first);
        let angle = (1.0 - distance).clamp(-1.0, 1.0).acos();
        let (s_, c_) = angle.sin_cos();
        let v: Vec<f32> = (0..first.len())
            .map(|k| s_ * orth.d[k] + c_ * first[k])
            .collect();
        Embedder::l2_normalize(&v).to_vec()
    }
}

pub fn at_distance_in(
    frame: &Frame,
    distance: f32,
    away: &mut Rng,
    sign: f32,
    noise: bool,
) -> Vec<f32> {
    let a = &frame.origin;
    let d = &frame.d;
    // v = cos(t)*a + sin(t)*d sits at cosine distance 1 - cos(t) from a, so the
    // angle is t = acos(1 - distance) -- the exact relation, not the small-angle
    // one. Using the approximation here would be a fixture that is subtly wrong
    // in the regime these tests care about (0.1 to 0.4, where 1 - cos(t) is
    // already 20-30% off t^2/2) and wrong in a direction that depends on the
    // distance, which is the kind of error a reviewer cannot see.
    let angle = (1.0 - distance).clamp(-1.0, 1.0).acos() * sign;
    let (s_, c_) = angle.sin_cos();
    let v: Vec<f32> = (0..a.len())
        .map(|k| {
            s_ * d[k]
                + c_ * a[k]
                + if noise {
                    (0.004 * away.normal()) as f32
                } else {
                    0.0
                }
        })
        .collect();
    Embedder::l2_normalize(&v).to_vec()
}

/// A unit vector at `distance` from the frame's origin, for building a second
/// group. A fresh `Frame` per group is what makes the two groups independent --
/// building both from one frame put them on the same arc and they merged.
pub fn frame_at(distance: f32, origin: &[f32], rng: &mut Rng) -> Vec<f32> {
    Frame::new(origin).at(distance, rng, 1.0)
}

/// A seeded generator, for callers that only need deterministic noise.
pub fn rng2(seed: u64) -> Rng {
    Rng::new(seed)
}

/// The face distance two vectors sit at, as the engine computes it.
pub fn distance(a: &[f32], b: &[f32]) -> f32 {
    let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
    (1.0 - dot).max(0.0)
}

/// The clusters in the store, with their member appearance ids.
pub async fn clusters(store: &Store) -> Vec<(String, Vec<String>)> {
    cluster::all_with_members(store)
        .await
        .unwrap()
        .into_iter()
        .map(|c| (c.id, c.member_ids))
        .collect()
}

// ---------------------------------------------------------------------------
// (1) The ten clusters are recovered.
// ---------------------------------------------------------------------------
