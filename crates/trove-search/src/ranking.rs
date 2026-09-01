const MILLIS_PER_DAY: f64 = 86_400_000.0;

#[derive(Clone, Copy)]
pub struct RankingWeights {
    pub lexical: f64,
    pub recency: f64,
    pub frequency: f64,
    pub pin_bonus: f64,
    pub recency_half_life_days: f64,
}

impl Default for RankingWeights {
    fn default() -> Self {
        Self {
            lexical: 1.0,
            recency: 0.35,
            frequency: 0.08,
            pin_bonus: 0.40,
            recency_half_life_days: 30.0,
        }
    }
}

#[derive(Clone, Copy)]
pub struct RankingSignals {
    pub bm25: f64,
    pub captured_at_ms: i64,
    pub occurrence_count: u64,
    pub paste_count: u64,
    pub pinned: bool,
}

pub fn rank_score(weights: RankingWeights, signals: RankingSignals, now_ms: i64) -> f64 {
    let lexical_strength = if signals.bm25.is_finite() {
        (-signals.bm25).max(0.0)
    } else {
        0.0
    };
    let normalized_lexical = lexical_strength / (1.0 + lexical_strength);
    let age_ms = now_ms.saturating_sub(signals.captured_at_ms).max(0) as f64;
    let half_life_ms =
        (weights.recency_half_life_days.max(f64::EPSILON) * MILLIS_PER_DAY).max(f64::EPSILON);
    let recency = 2.0_f64.powf(-age_ms / half_life_ms);
    let frequency = signals.occurrence_count.saturating_add(signals.paste_count) as f64;

    weights.lexical * normalized_lexical
        + weights.recency * recency
        + weights.frequency * frequency.ln_1p()
        + if signals.pinned {
            weights.pin_bonus
        } else {
            0.0
        }
}
