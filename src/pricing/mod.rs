use std::collections::HashMap;

use serde::{Deserialize, Serialize};

/// Where the cost data for a usage record came from
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum CostPricingSource {
    /// Upstream API reported cost (e.g. OpenRouter's `cost` field)
    Provider,
    /// From `[providers.*.models]` in hadrian.toml
    ProviderConfig,
    /// From `[pricing]` section in hadrian.toml
    PricingConfig,
    /// From models.dev catalog
    Catalog,
    /// No cost available
    #[default]
    None,
}

impl CostPricingSource {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Provider => "provider",
            Self::ProviderConfig => "provider_config",
            Self::PricingConfig => "pricing_config",
            Self::Catalog => "catalog",
            Self::None => "none",
        }
    }

    pub fn parse(s: &str) -> Self {
        match s {
            "provider" => Self::Provider,
            "provider_config" => Self::ProviderConfig,
            "pricing_config" => Self::PricingConfig,
            "catalog" => Self::Catalog,
            _ => Self::None,
        }
    }
}

impl std::fmt::Display for CostPricingSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Source preference for cost calculation
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum CostSource {
    /// Prefer provider-reported cost (from API response), fall back to calculated
    #[default]
    PreferProvider,
    /// Always use calculated cost based on configured pricing
    CalculatedOnly,
    /// Always use provider-reported cost, fail if not available
    ProviderOnly,
}

/// Pricing information for a specific model.
///
/// Costs are stored in microcents (1/10000 of a cent) for precision.
/// For example, $0.000002 per token = 0.0002 cents = 0.02 microcents.
///
/// This allows representing very small costs like:
/// - OpenRouter's Gemini 3 Pro: $0.000002/token = 0.02 microcents
/// - Cache read pricing: $0.0000002/token = 0.002 microcents
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct ModelPricing {
    /// Cost per 1M input tokens in microcents (divide by 10000 for cents)
    /// Using per-1M to match provider APIs and avoid floating point
    #[serde(default)]
    pub input_per_1m_tokens: i64,

    /// Cost per 1M output tokens in microcents
    #[serde(default)]
    pub output_per_1m_tokens: i64,

    /// Cost per image (for vision models) in microcents (fallback for image_pricing)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub per_image: Option<i64>,

    /// Per-image cost by quality and size, keyed as `"quality:size"` (e.g. `"hd:1024x1024"`).
    /// Supports wildcards: `"*:1024x1024"`, `"hd:*"`, `"*:*"`.
    /// Falls back to `per_image` when no key matches.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image_pricing: Option<HashMap<String, i64>>,

    /// Cost per request in microcents (some providers charge per-request)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub per_request: Option<i64>,

    /// Cost per 1M cached input tokens (for providers that support caching)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cached_input_per_1m_tokens: Option<i64>,

    /// Cost per 1M cache write tokens
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_write_per_1m_tokens: Option<i64>,

    /// Cost per 1M internal reasoning tokens (for reasoning models)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_per_1m_tokens: Option<i64>,

    /// Cost per second of audio (for transcription/translation) in microcents
    /// Example: Whisper at $0.006/min = $0.0001/sec = 100 microcents/sec
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub per_second: Option<i64>,

    /// Cost per 1M characters (for TTS) in microcents
    /// Example: tts-1 at $0.015/1K chars = $15/1M chars = 15_000_000 microcents/1M
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub per_1m_characters: Option<i64>,
}

impl ModelPricing {
    /// Create pricing from per-1k-token costs in cents (legacy format)
    ///
    /// The input values are in HUNDREDTHS of a cent (0.01 cent) per 1k tokens.
    /// This was the old format where:
    /// - 3000 = $0.03/1k = 3 cents/1k
    /// - 50 = $0.0005/1k = 0.05 cents/1k
    ///
    /// Convert to microcents per 1M:
    /// value * 1000 (1k->1M) * 100 (hundredths->microcents) = value * 100_000
    pub fn from_cents_per_1k(input_per_1k: i64, output_per_1k: i64) -> Self {
        Self {
            input_per_1m_tokens: input_per_1k * 100_000,
            output_per_1m_tokens: output_per_1k * 100_000,
            ..Default::default()
        }
    }

    /// Create pricing from per-token costs in dollars (provider API format like OpenRouter)
    /// OpenRouter uses strings like "0.000002" for $/token
    ///
    /// Example: $0.000002/token = $2/1M tokens = 200 cents/1M = 2_000_000 microcents/1M
    pub fn from_dollars_per_token(input: f64, output: f64) -> Self {
        // $/token -> microcents/1M
        // $/token * 1_000_000 tokens * 100 cents/$ * 10000 microcents/cent
        // = $/token * 1_000_000_000_000
        let input_microcents = (input * 1_000_000_000_000.0).round() as i64;
        let output_microcents = (output * 1_000_000_000_000.0).round() as i64;

        Self {
            input_per_1m_tokens: input_microcents,
            output_per_1m_tokens: output_microcents,
            ..Default::default()
        }
    }

    /// Create pricing for image generation models (per-image pricing)
    ///
    /// Example: DALL-E 3 at $0.04/image = 40_000 microcents/image
    pub fn from_dollars_per_image(dollars_per_image: f64) -> Self {
        Self {
            per_image: Some(dollars_to_microcents(dollars_per_image)),
            ..Default::default()
        }
    }

    /// Create pricing from an image pricing table
    ///
    /// Keys are `"quality:size"` strings (e.g. `"hd:1024x1024"`).
    /// Supports wildcards: `"*:1024x1024"`, `"hd:*"`, `"*:*"`.
    pub fn from_image_pricing_table(table: HashMap<String, i64>) -> Self {
        Self {
            image_pricing: Some(table),
            ..Default::default()
        }
    }

    /// Resolve per-image price using the image_pricing table with wildcard fallback.
    ///
    /// Lookup order:
    /// 1. Exact match: `"hd:1024x1024"`
    /// 2. Quality wildcard: `"*:1024x1024"`
    /// 3. Size wildcard: `"hd:*"`
    /// 4. Full wildcard: `"*:*"`
    /// 5. Fallback: `per_image`
    pub fn resolve_image_price(&self, quality: Option<&str>, size: Option<&str>) -> Option<i64> {
        if let Some(table) = &self.image_pricing {
            let q = quality.unwrap_or("*");
            let s = size.unwrap_or("*");

            // 1. Exact match
            let exact = format!("{q}:{s}");
            if let Some(&price) = table.get(&exact) {
                return Some(price);
            }
            // 2. Quality wildcard
            let qw = format!("*:{s}");
            if let Some(&price) = table.get(&qw) {
                return Some(price);
            }
            // 3. Size wildcard
            let sw = format!("{q}:*");
            if let Some(&price) = table.get(&sw) {
                return Some(price);
            }
            // 4. Full wildcard
            if let Some(&price) = table.get("*:*") {
                return Some(price);
            }
        }
        // 5. Fallback
        self.per_image
    }

    /// Create pricing for audio transcription/translation (per-minute pricing)
    ///
    /// Example: Whisper at $0.006/min = 6_000 microcents/min = 100 microcents/sec
    pub fn from_dollars_per_minute(dollars_per_minute: f64) -> Self {
        // Convert $/min to microcents/sec
        let microcents_per_minute = dollars_to_microcents(dollars_per_minute);
        let microcents_per_second = microcents_per_minute / 60;
        Self {
            per_second: Some(microcents_per_second),
            ..Default::default()
        }
    }

    /// Create pricing for TTS (per-character pricing)
    ///
    /// Example: tts-1 at $0.015/1K chars = $15/1M chars = 15_000_000 microcents/1M
    pub fn from_dollars_per_1k_characters(dollars_per_1k: f64) -> Self {
        // Convert $/1K to microcents/1M
        let microcents_per_1m = dollars_to_microcents(dollars_per_1k * 1000.0);
        Self {
            per_1m_characters: Some(microcents_per_1m),
            ..Default::default()
        }
    }
}

/// Token usage breakdown for cost calculation
#[derive(Debug, Clone, Default)]
pub struct TokenUsage {
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cached_tokens: Option<i64>,
    pub reasoning_tokens: Option<i64>,
    pub image_count: Option<i64>,
    /// Image size string (e.g. `"1024x1024"`) for size-aware pricing
    pub image_size: Option<String>,
    /// Image quality string (e.g. `"hd"`) for quality-aware pricing
    pub image_quality: Option<String>,
    /// Audio duration in seconds (for transcription/translation pricing)
    pub audio_seconds: Option<i64>,
    /// Character count (for TTS pricing)
    pub character_count: Option<i64>,
    /// Video duration in seconds (for video-generation pricing)
    pub video_seconds: Option<i64>,
}

impl TokenUsage {
    pub fn new(input_tokens: i64, output_tokens: i64) -> Self {
        Self {
            input_tokens,
            output_tokens,
            ..Default::default()
        }
    }

    /// Create usage for image generation with optional size and quality for
    /// size/quality-aware pricing lookup.
    pub fn for_images(image_count: i64, size: Option<&str>, quality: Option<&str>) -> Self {
        Self {
            image_count: Some(image_count),
            image_size: size.map(String::from),
            image_quality: quality.map(String::from),
            ..Default::default()
        }
    }

    /// Create usage for audio transcription/translation
    pub fn for_audio_seconds(seconds: i64) -> Self {
        Self {
            audio_seconds: Some(seconds),
            ..Default::default()
        }
    }

    /// Create usage for TTS (text-to-speech)
    pub fn for_tts_characters(characters: i64) -> Self {
        Self {
            character_count: Some(characters),
            ..Default::default()
        }
    }

    /// Create usage for video generation (per-second pricing)
    pub fn for_video_seconds(seconds: i64) -> Self {
        Self {
            video_seconds: Some(seconds),
            ..Default::default()
        }
    }
}

/// Pricing configuration for all providers and models
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct PricingConfig {
    /// Pricing by provider and model
    /// Structure: pricing[provider][model] = ModelPricing
    #[serde(default)]
    pub pricing: HashMap<String, HashMap<String, ModelPricing>>,

    /// Cost source preference for usage tracking
    #[serde(default)]
    pub cost_source: CostSource,

    /// Runtime catalog for fallback pricing lookups (not serialized)
    #[serde(skip)]
    #[cfg_attr(feature = "json-schema", schemars(skip))]
    catalog: Option<crate::catalog::ModelCatalogRegistry>,

    /// Maps Hadrian provider names to catalog provider IDs (not serialized)
    #[serde(skip)]
    #[cfg_attr(feature = "json-schema", schemars(skip))]
    provider_catalog_map: HashMap<String, String>,

    /// Tracks the source of each pricing entry: source_map[provider][model] -> CostPricingSource
    #[serde(skip)]
    #[cfg_attr(feature = "json-schema", schemars(skip))]
    source_map: HashMap<String, HashMap<String, CostPricingSource>>,
}

impl PricingConfig {
    /// Get pricing for a specific provider and model
    pub fn get(&self, provider: &str, model: &str) -> Option<&ModelPricing> {
        self.pricing.get(provider)?.get(model)
    }

    /// Calculate cost in microcents for given token usage (simple version)
    pub fn calculate_cost(
        &self,
        provider: &str,
        model: &str,
        input_tokens: i64,
        output_tokens: i64,
    ) -> Option<(i64, CostPricingSource)> {
        self.calculate_cost_detailed(
            provider,
            model,
            &TokenUsage::new(input_tokens, output_tokens),
        )
    }

    /// Calculate cost in microcents for detailed token usage
    ///
    /// Returns `(cost_microcents, source)` where source indicates where
    /// the pricing data came from.
    ///
    /// First checks the pre-populated pricing HashMap, then falls back to
    /// a runtime catalog lookup for models not in `allowed_models`.
    pub fn calculate_cost_detailed(
        &self,
        provider: &str,
        model: &str,
        usage: &TokenUsage,
    ) -> Option<(i64, CostPricingSource)> {
        if let Some(pricing) = self.get(provider, model) {
            let source = self.get_source(provider, model);
            return Some((Self::compute_cost(pricing, usage), source));
        }
        if let Some(pricing) = self.lookup_catalog(provider, model) {
            return Some((
                Self::compute_cost(&pricing, usage),
                CostPricingSource::Catalog,
            ));
        }
        None
    }

    /// Look up pricing from the runtime catalog for a provider/model pair.
    fn lookup_catalog(&self, provider: &str, model: &str) -> Option<ModelPricing> {
        let catalog = self.catalog.as_ref()?;
        let catalog_provider_id = self.provider_catalog_map.get(provider)?;
        catalog.get_pricing(catalog_provider_id, model)
    }

    /// Compute cost in microcents from pricing and token usage.
    ///
    /// Uses `i128` for intermediate calculations to prevent overflow with
    /// large token counts (billions of tokens) and high pricing values.
    /// Results are saturated to `i64::MAX` if they would overflow.
    fn compute_cost(pricing: &ModelPricing, usage: &TokenUsage) -> i64 {
        let mut total_microcents: i128 = 0;

        // Input tokens (subtract cached if applicable)
        let regular_input = usage
            .cached_tokens
            .map(|c| usage.input_tokens.saturating_sub(c))
            .unwrap_or(usage.input_tokens);
        total_microcents +=
            (regular_input as i128 * pricing.input_per_1m_tokens as i128) / 1_000_000;

        // Output tokens
        total_microcents +=
            (usage.output_tokens as i128 * pricing.output_per_1m_tokens as i128) / 1_000_000;

        // Cached input tokens (if pricing available)
        if let (Some(cached), Some(cached_price)) =
            (usage.cached_tokens, pricing.cached_input_per_1m_tokens)
        {
            total_microcents += (cached as i128 * cached_price as i128) / 1_000_000;
        }

        // Reasoning tokens
        if let (Some(reasoning), Some(reasoning_price)) =
            (usage.reasoning_tokens, pricing.reasoning_per_1m_tokens)
        {
            total_microcents += (reasoning as i128 * reasoning_price as i128) / 1_000_000;
        }

        // Per-image cost (with size/quality-aware lookup)
        if let Some(images) = usage.image_count
            && let Some(image_price) = pricing
                .resolve_image_price(usage.image_quality.as_deref(), usage.image_size.as_deref())
        {
            total_microcents += images as i128 * image_price as i128;
        }

        // Per-second cost (audio transcription/translation)
        if let (Some(seconds), Some(second_price)) = (usage.audio_seconds, pricing.per_second) {
            total_microcents += seconds as i128 * second_price as i128;
        }

        // Per-second cost (video generation). Reuses the model's `per_second`
        // rate; a video model never also reports `audio_seconds`.
        if let (Some(seconds), Some(second_price)) = (usage.video_seconds, pricing.per_second) {
            total_microcents += seconds as i128 * second_price as i128;
        }

        // Per-character cost (TTS)
        if let (Some(chars), Some(char_price)) = (usage.character_count, pricing.per_1m_characters)
        {
            total_microcents += (chars as i128 * char_price as i128) / 1_000_000;
        }

        // Per-request cost
        if let Some(request_price) = pricing.per_request {
            total_microcents += request_price as i128;
        }

        // Saturate to i64::MAX if result would overflow
        saturate_to_i64(total_microcents)
    }

    /// Add or update pricing for a model with source tracking
    pub fn set_pricing_with_source(
        &mut self,
        provider: &str,
        model: &str,
        pricing: ModelPricing,
        source: CostPricingSource,
    ) {
        self.pricing
            .entry(provider.to_string())
            .or_default()
            .insert(model.to_string(), pricing);
        self.source_map
            .entry(provider.to_string())
            .or_default()
            .insert(model.to_string(), source);
    }

    /// Add or update pricing for a model (without source tracking)
    pub fn set_pricing(&mut self, provider: &str, model: &str, pricing: ModelPricing) {
        self.set_pricing_with_source(provider, model, pricing, CostPricingSource::None);
    }

    /// Get the pricing source for a specific provider and model
    pub fn get_source(&self, provider: &str, model: &str) -> CostPricingSource {
        self.source_map
            .get(provider)
            .and_then(|models| models.get(model))
            .copied()
            .unwrap_or(CostPricingSource::None)
    }

    /// Merge another pricing config into this one (other takes precedence)
    pub fn merge(&mut self, other: &PricingConfig) {
        for (provider, models) in &other.pricing {
            for (model, pricing) in models {
                self.set_pricing_with_source(
                    provider,
                    model,
                    pricing.clone(),
                    CostPricingSource::PricingConfig,
                );
            }
        }
    }

    /// Merge pricing from provider configurations
    pub fn merge_provider_config(&mut self, providers: &crate::config::ProvidersConfig) {
        for (provider_name, provider_config) in providers.iter() {
            for (model_name, model_config) in provider_config.models() {
                self.set_pricing_with_source(
                    provider_name,
                    model_name,
                    model_config.pricing.clone(),
                    CostPricingSource::ProviderConfig,
                );
            }
        }
    }

    /// Merge pricing from the model catalog.
    ///
    /// This loads pricing for all models in the catalog, keyed by the Hadrian
    /// provider name (not the catalog provider ID). The caller must provide
    /// a mapping from provider names to catalog provider IDs.
    pub fn merge_catalog(
        &mut self,
        catalog: &crate::catalog::ModelCatalogRegistry,
        providers: &crate::config::ProvidersConfig,
    ) {
        for (provider_name, provider_config) in providers.iter() {
            // Resolve the catalog provider ID for this Hadrian provider
            let catalog_provider_id = crate::catalog::resolve_catalog_provider_id(
                provider_config.provider_type_name(),
                provider_config.base_url(),
                provider_config.catalog_provider(),
            );

            if let Some(catalog_pid) = catalog_provider_id {
                // For each model the provider might have, check if catalog has pricing
                // Since we don't know all models upfront, we need a different approach:
                // The catalog lookup is done at runtime when calculating cost.
                // Here we can pre-populate for known models if the provider has an allowed_models list.
                for model_name in provider_config.allowed_models() {
                    if let Some(enrichment) = catalog.lookup(&catalog_pid, model_name) {
                        // Only set if not already configured
                        if self.get(provider_name, model_name).is_none() {
                            self.set_pricing_with_source(
                                provider_name,
                                model_name,
                                enrichment.pricing.clone(),
                                CostPricingSource::Catalog,
                            );
                        }
                    }
                }
            }
        }
    }

    /// Create a pricing config from config file + provider configs + optional catalog
    pub fn from_config(config: &PricingConfig, providers: &crate::config::ProvidersConfig) -> Self {
        Self::from_config_with_catalog(config, providers, None)
    }

    /// Create a pricing config with catalog fallback.
    ///
    /// Priority chain (highest to lowest):
    /// 1. Per-provider `models` in provider config
    /// 2. Explicit `[pricing]` section in config
    /// 3. models.dev catalog pricing (pre-populated for `allowed_models`, runtime fallback for others)
    /// 4. At runtime: provider-reported cost (via `CostSource::PreferProvider`)
    pub fn from_config_with_catalog(
        config: &PricingConfig,
        providers: &crate::config::ProvidersConfig,
        catalog: Option<&crate::catalog::ModelCatalogRegistry>,
    ) -> Self {
        // Build provider-name → catalog-provider-ID mapping for runtime fallback
        let mut provider_catalog_map = HashMap::new();
        for (provider_name, provider_config) in providers.iter() {
            if let Some(catalog_pid) = crate::catalog::resolve_catalog_provider_id(
                provider_config.provider_type_name(),
                provider_config.base_url(),
                provider_config.catalog_provider(),
            ) {
                provider_catalog_map.insert(provider_name.to_string(), catalog_pid);
            }
        }

        let mut result = Self {
            cost_source: config.cost_source,
            catalog: catalog.cloned(),
            provider_catalog_map,
            ..Default::default()
        };

        // First: load catalog pricing as base layer for allowed_models (lowest priority)
        if let Some(cat) = catalog {
            result.merge_catalog(cat, providers);
        }

        // Second: merge explicit pricing config (overrides catalog)
        result.merge(config);

        // Third: merge provider-specific pricing (highest priority, overrides everything)
        result.merge_provider_config(providers);

        result
    }

    /// Resolve cost based on cost_source preference
    ///
    /// Takes provider-reported cost (in dollars) and calculated cost with source.
    /// Returns `(cost_microcents, pricing_source)` based on the configured preference.
    pub fn resolve_cost(
        &self,
        provider_cost_dollars: Option<f64>,
        calculated: Option<(i64, CostPricingSource)>,
    ) -> (Option<i64>, CostPricingSource) {
        match self.cost_source {
            CostSource::PreferProvider => {
                if let Some(dollars) = provider_cost_dollars {
                    (
                        Some(dollars_to_microcents(dollars)),
                        CostPricingSource::Provider,
                    )
                } else if let Some((cost, src)) = calculated {
                    (Some(cost), src)
                } else {
                    (None, CostPricingSource::None)
                }
            }
            CostSource::CalculatedOnly => {
                if let Some((cost, src)) = calculated {
                    (Some(cost), src)
                } else {
                    (None, CostPricingSource::None)
                }
            }
            CostSource::ProviderOnly => {
                if let Some(dollars) = provider_cost_dollars {
                    (
                        Some(dollars_to_microcents(dollars)),
                        CostPricingSource::Provider,
                    )
                } else {
                    (None, CostPricingSource::None)
                }
            }
        }
    }
}

/// Convert dollars to microcents (1/1,000,000 of a dollar)
///
/// Examples:
/// - $1.00 = 1,000,000 microcents
/// - $0.01 = 10,000 microcents
/// - $0.000207 = 207 microcents
pub fn dollars_to_microcents(dollars: f64) -> i64 {
    (dollars * 1_000_000.0).round() as i64
}

/// Convert microcents to dollars
pub fn microcents_to_dollars(microcents: i64) -> f64 {
    microcents as f64 / 1_000_000.0
}

/// Saturate an i128 value to fit in an i64
///
/// Returns `i64::MAX` if the value exceeds the i64 range, or the value as i64 otherwise.
/// Negative values that underflow return `i64::MIN`.
fn saturate_to_i64(value: i128) -> i64 {
    if value > i64::MAX as i128 {
        i64::MAX
    } else if value < i64::MIN as i128 {
        i64::MIN
    } else {
        value as i64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_from_dollars_per_token() {
        // OpenRouter format: "0.000002" = $0.000002 per token
        let pricing = ModelPricing::from_dollars_per_token(0.000002, 0.000012);
        // $0.000002/token * 1M tokens * 100 cents/$ * 10000 microcents/cent = 2_000_000 microcents/1M
        assert_eq!(pricing.input_per_1m_tokens, 2_000_000);
        assert_eq!(pricing.output_per_1m_tokens, 12_000_000);
    }

    #[test]
    fn test_from_cents_per_1k() {
        // Legacy format: 100 (hundredths of cents) per 1k tokens = 1 cent per 1k = 1000 cents per 1M
        let pricing = ModelPricing::from_cents_per_1k(100, 200);
        // 100 * 100_000 = 10_000_000 microcents/1M
        assert_eq!(pricing.input_per_1m_tokens, 10_000_000);
        assert_eq!(pricing.output_per_1m_tokens, 20_000_000);
    }

    #[test]
    fn test_calculate_cost() {
        let mut config = PricingConfig::default();

        // Set up pricing: 30 cents/1k input, 60 cents/1k output
        // 3000 * 100_000 = 300_000_000 microcents/1M input
        // 6000 * 100_000 = 600_000_000 microcents/1M output
        config.set_pricing(
            "openai",
            "gpt-4",
            ModelPricing::from_cents_per_1k(3000, 6000),
        );

        // 1000 input: 1000 * 300_000_000 / 1_000_000 = 300_000 microcents
        // 500 output: 500 * 600_000_000 / 1_000_000 = 300_000 microcents
        // Total: 600_000 microcents = $0.60
        let cost = config.calculate_cost("openai", "gpt-4", 1000, 500);
        assert_eq!(cost.map(|(c, _)| c), Some(600_000));
    }

    #[test]
    fn test_calculate_cost_unknown_model() {
        let config = PricingConfig::default();
        let cost = config.calculate_cost("openai", "nonexistent-model", 1000, 1000);
        assert_eq!(cost, None);
    }

    #[test]
    fn test_calculate_cost_unknown_provider() {
        let config = PricingConfig::default();
        let cost = config.calculate_cost("unknown-provider", "gpt-4", 1000, 1000);
        assert_eq!(cost, None);
    }

    #[test]
    fn test_set_pricing() {
        let mut config = PricingConfig::default();

        // 100 cents per 1M input, 200 cents per 1M output (in microcents = cents * 10000)
        config.set_pricing(
            "custom",
            "my-model",
            ModelPricing {
                input_per_1m_tokens: 100 * 10000, // 1_000_000 microcents = 100 cents = $1
                output_per_1m_tokens: 200 * 10000, // 2_000_000 microcents = 200 cents = $2
                ..Default::default()
            },
        );

        // 1M tokens each: 1_000_000 + 2_000_000 = 3_000_000 microcents = $3
        let cost = config.calculate_cost("custom", "my-model", 1_000_000, 1_000_000);
        assert_eq!(cost.map(|(c, _)| c), Some(3_000_000));
    }

    #[test]
    fn test_large_token_counts() {
        let mut config = PricingConfig::default();

        // 30 cents/1k input, 60 cents/1k output
        config.set_pricing(
            "openai",
            "gpt-4",
            ModelPricing::from_cents_per_1k(3000, 6000),
        );

        // 100k input: 100 * 300_000 = 30_000_000 microcents
        // 4k output: 4 * 600_000 = 2_400_000 microcents
        // Total: 32_400_000 microcents = $32.40
        let cost = config.calculate_cost("openai", "gpt-4", 100_000, 4_000);
        assert_eq!(cost.map(|(c, _)| c), Some(32_400_000));
    }

    #[test]
    fn test_detailed_usage_with_caching() {
        let mut config = PricingConfig::default();

        // Pricing: $1 per 1M input, $2 per 1M output, $0.10 per 1M cached
        // In microcents: 100 cents * 10000 = 1_000_000, etc.
        config.set_pricing(
            "test",
            "cached-model",
            ModelPricing {
                input_per_1m_tokens: 100 * 10000, // 1_000_000 microcents = $1 per 1M
                output_per_1m_tokens: 200 * 10000, // 2_000_000 microcents = $2 per 1M
                cached_input_per_1m_tokens: Some(10 * 10000), // 100_000 microcents = $0.10 per 1M
                ..Default::default()
            },
        );

        // 1M input tokens, 500k cached, 500k output
        let usage = TokenUsage {
            input_tokens: 1_000_000,
            output_tokens: 500_000,
            cached_tokens: Some(500_000),
            ..Default::default()
        };

        // Regular input (1M - 500k = 500k): 500k * 1_000_000 / 1M = 500_000 microcents
        // Cached input: 500k * 100_000 / 1M = 50_000 microcents
        // Output: 500k * 2_000_000 / 1M = 1_000_000 microcents
        // Total: 1_550_000 microcents = $1.55
        let cost = config.calculate_cost_detailed("test", "cached-model", &usage);
        assert_eq!(cost.map(|(c, _)| c), Some(1_550_000));
    }

    #[test]
    fn test_video_per_second_cost() {
        let mut config = PricingConfig::default();
        // $0.10/second of output video.
        config.set_pricing(
            "openai",
            "sora-2",
            ModelPricing {
                per_second: Some(100_000), // 100_000 microcents = $0.10
                ..Default::default()
            },
        );

        // An 8-second clip: 8 * 100_000 = 800_000 microcents = $0.80.
        let usage = TokenUsage::for_video_seconds(8);
        let cost = config.calculate_cost_detailed("openai", "sora-2", &usage);
        assert_eq!(cost.map(|(c, _)| c), Some(800_000));
    }

    #[test]
    fn test_merge_configs() {
        let mut base = PricingConfig::default();
        let mut overlay = PricingConfig::default();

        // Set base pricing
        base.set_pricing(
            "openai",
            "gpt-4",
            ModelPricing::from_cents_per_1k(3000, 6000),
        );

        // Override with cheaper pricing: 10 cents/1k input, 20 cents/1k output
        overlay.set_pricing(
            "openai",
            "gpt-4",
            ModelPricing::from_cents_per_1k(1000, 2000),
        );

        base.merge(&overlay);

        // 1000 input + 1000 output with new pricing
        // 1000 * 100_000_000 / 1_000_000 = 100_000 microcents input
        // 1000 * 200_000_000 / 1_000_000 = 200_000 microcents output
        // Total: 300_000 microcents = $0.30
        let cost = base.calculate_cost("openai", "gpt-4", 1000, 1000);
        assert_eq!(cost.map(|(c, _)| c), Some(300_000));
    }

    #[test]
    fn test_dollars_to_microcents() {
        assert_eq!(dollars_to_microcents(1.0), 1_000_000);
        assert_eq!(dollars_to_microcents(0.01), 10_000); // 1 cent
        assert_eq!(dollars_to_microcents(0.001), 1_000); // 0.1 cent
        assert_eq!(dollars_to_microcents(0.000001), 1); // 1 microcent
        assert_eq!(dollars_to_microcents(0.000207), 207); // OpenRouter example
        assert_eq!(dollars_to_microcents(0.012345), 12_345);
        assert_eq!(dollars_to_microcents(10.50), 10_500_000);
    }

    #[test]
    fn test_resolve_cost_prefer_provider() {
        let config = PricingConfig {
            cost_source: CostSource::PreferProvider,
            ..Default::default()
        };

        // Provider cost available - use it ($0.05 = 50_000 microcents)
        let (cost, src) =
            config.resolve_cost(Some(0.05), Some((30_000, CostPricingSource::Catalog)));
        assert_eq!(cost, Some(50_000));
        assert_eq!(src, CostPricingSource::Provider);

        // No provider cost - fall back to calculated
        let (cost, src) = config.resolve_cost(None, Some((30_000, CostPricingSource::Catalog)));
        assert_eq!(cost, Some(30_000));
        assert_eq!(src, CostPricingSource::Catalog);

        // Neither available
        let (cost, src) = config.resolve_cost(None, None);
        assert_eq!(cost, None);
        assert_eq!(src, CostPricingSource::None);
    }

    #[test]
    fn test_resolve_cost_calculated_only() {
        let config = PricingConfig {
            cost_source: CostSource::CalculatedOnly,
            ..Default::default()
        };

        // Always use calculated, even when provider cost available
        let (cost, src) =
            config.resolve_cost(Some(0.05), Some((30_000, CostPricingSource::PricingConfig)));
        assert_eq!(cost, Some(30_000));
        assert_eq!(src, CostPricingSource::PricingConfig);
        let (cost, _) = config.resolve_cost(None, Some((30_000, CostPricingSource::PricingConfig)));
        assert_eq!(cost, Some(30_000));
        let (cost, src) = config.resolve_cost(Some(0.05), None);
        assert_eq!(cost, None);
        assert_eq!(src, CostPricingSource::None);
    }

    #[test]
    fn test_resolve_cost_provider_only() {
        let config = PricingConfig {
            cost_source: CostSource::ProviderOnly,
            ..Default::default()
        };

        // Always use provider cost, ignore calculated ($0.05 = 50_000 microcents)
        let (cost, src) =
            config.resolve_cost(Some(0.05), Some((30_000, CostPricingSource::Catalog)));
        assert_eq!(cost, Some(50_000));
        assert_eq!(src, CostPricingSource::Provider);
        let (cost, src) = config.resolve_cost(None, Some((30_000, CostPricingSource::Catalog)));
        assert_eq!(cost, None);
        assert_eq!(src, CostPricingSource::None);
        let (cost, src) = config.resolve_cost(Some(0.05), None);
        assert_eq!(cost, Some(50_000));
        assert_eq!(src, CostPricingSource::Provider);
    }

    #[test]
    fn test_saturate_to_i64() {
        // Normal values pass through
        assert_eq!(saturate_to_i64(0), 0);
        assert_eq!(saturate_to_i64(1_000_000), 1_000_000);
        assert_eq!(saturate_to_i64(-1_000_000), -1_000_000);

        // i64 boundaries pass through
        assert_eq!(saturate_to_i64(i64::MAX as i128), i64::MAX);
        assert_eq!(saturate_to_i64(i64::MIN as i128), i64::MIN);

        // Values beyond i64::MAX saturate
        assert_eq!(saturate_to_i64(i64::MAX as i128 + 1), i64::MAX);
        assert_eq!(saturate_to_i64(i128::MAX), i64::MAX);

        // Values below i64::MIN saturate
        assert_eq!(saturate_to_i64(i64::MIN as i128 - 1), i64::MIN);
        assert_eq!(saturate_to_i64(i128::MIN), i64::MIN);
    }

    #[test]
    fn test_overflow_protection_large_token_counts() {
        let mut config = PricingConfig::default();

        // Set up expensive pricing: $60/1k = $60,000/1M output tokens
        // 6000 (hundredths of cents per 1k) * 100_000 = 600_000_000 microcents/1M
        config.set_pricing(
            "openai",
            "gpt-4",
            ModelPricing::from_cents_per_1k(3000, 6000),
        );

        // 10 billion tokens at $60/1k = $600M total
        // 10B * 600M microcents/1M / 1M = 6,000,000,000,000 microcents = $6,000,000
        // Wait, $60/1k = $60,000/1M, so 10B tokens = 10,000 * 1M tokens
        // Cost = 10,000 * $60,000 = $600,000,000 = 600B cents = 6T microcents? No...
        // Let's recalc: 6000 hundredths of cents per 1k = 60 cents per 1k = $0.60/1k = $600/1M
        // 10B tokens = 10,000 * 1M tokens, cost = 10,000 * $600 = $6,000,000 = 6_000_000_000_000 microcents
        let cost = config.calculate_cost("openai", "gpt-4", 0, 10_000_000_000);
        assert_eq!(cost.map(|(c, _)| c), Some(6_000_000_000_000)); // $6M in microcents

        // Now test with even larger values that would overflow without i128
        // 100 trillion tokens (unrealistic but tests overflow)
        // 100T * 600M = 6 * 10^22, definitely overflows i64
        // After division by 1M: 6 * 10^16, still fits in i64
        let huge_cost = config.calculate_cost("openai", "gpt-4", 0, 100_000_000_000_000);
        assert_eq!(huge_cost.map(|(c, _)| c), Some(60_000_000_000_000_000)); // $60B in microcents
    }

    #[test]
    fn test_overflow_protection_extreme_values() {
        let mut config = PricingConfig::default();

        // Extremely high pricing that would cause overflow with i64 multiplication
        // $1000/token = $1B/1M tokens = 100B cents/1M = 1_000_000_000_000_000 microcents/1M
        config.set_pricing(
            "expensive",
            "extreme-model",
            ModelPricing {
                input_per_1m_tokens: 0,
                output_per_1m_tokens: 1_000_000_000_000_000, // $1B per 1M tokens
                ..Default::default()
            },
        );

        // 100 billion tokens at $1000/token should saturate
        // 100B tokens * $1B/1M tokens = way more than i64::MAX
        let cost = config.calculate_cost("expensive", "extreme-model", 0, 100_000_000_000_000);
        // Should saturate to i64::MAX instead of wrapping to negative
        assert_eq!(cost.map(|(c, _)| c), Some(i64::MAX));
    }

    #[test]
    fn test_overflow_protection_multiple_components() {
        let mut config = PricingConfig::default();

        // High pricing with multiple cost components
        config.set_pricing(
            "test",
            "multi-cost",
            ModelPricing {
                input_per_1m_tokens: 500_000_000_000_000, // Very high
                output_per_1m_tokens: 500_000_000_000_000,
                reasoning_per_1m_tokens: Some(500_000_000_000_000),
                cached_input_per_1m_tokens: Some(100_000_000_000_000),
                cache_write_per_1m_tokens: None,
                per_image: Some(1_000_000_000_000),
                image_pricing: None,
                per_request: Some(1_000_000_000_000),
                per_second: None,
                per_1m_characters: None,
            },
        );

        // Large usage that would overflow if not protected
        let usage = TokenUsage {
            input_tokens: 10_000_000_000,
            output_tokens: 10_000_000_000,
            reasoning_tokens: Some(10_000_000_000),
            cached_tokens: Some(5_000_000_000),
            image_count: Some(1_000_000),
            image_size: None,
            image_quality: None,
            audio_seconds: None,
            character_count: None,
            video_seconds: None,
        };

        let cost = config.calculate_cost_detailed("test", "multi-cost", &usage);
        // Should saturate instead of overflow
        assert_eq!(cost.map(|(c, _)| c), Some(i64::MAX));
    }

    #[test]
    fn test_image_pricing() {
        let mut config = PricingConfig::default();

        // DALL-E 3 Standard 1024x1024: $0.04/image = 40_000 microcents
        config.set_pricing(
            "openai",
            "dall-e-3",
            ModelPricing::from_dollars_per_image(0.04),
        );

        // Generate 3 images (no size/quality → uses per_image fallback)
        let usage = TokenUsage::for_images(3, None, None);
        let cost = config.calculate_cost_detailed("openai", "dall-e-3", &usage);
        assert_eq!(cost.map(|(c, _)| c), Some(120_000)); // $0.12

        // DALL-E 2: $0.02/image = 20_000 microcents
        config.set_pricing(
            "openai",
            "dall-e-2",
            ModelPricing::from_dollars_per_image(0.02),
        );

        let usage = TokenUsage::for_images(5, None, None);
        let cost = config.calculate_cost_detailed("openai", "dall-e-2", &usage);
        assert_eq!(cost.map(|(c, _)| c), Some(100_000)); // $0.10
    }

    #[test]
    fn test_image_pricing_table_exact_match() {
        let mut config = PricingConfig::default();
        let mut table = HashMap::new();
        table.insert("standard:1024x1024".to_string(), 40_000);
        table.insert("hd:1024x1024".to_string(), 80_000);
        table.insert("hd:1792x1024".to_string(), 120_000);

        config.set_pricing(
            "openai",
            "dall-e-3",
            ModelPricing {
                per_image: Some(40_000),
                image_pricing: Some(table),
                ..Default::default()
            },
        );

        // Exact match: hd:1024x1024 → 80_000
        let usage = TokenUsage::for_images(1, Some("1024x1024"), Some("hd"));
        let cost = config.calculate_cost_detailed("openai", "dall-e-3", &usage);
        assert_eq!(cost.map(|(c, _)| c), Some(80_000));

        // Exact match: hd:1792x1024 → 120_000
        let usage = TokenUsage::for_images(2, Some("1792x1024"), Some("hd"));
        let cost = config.calculate_cost_detailed("openai", "dall-e-3", &usage);
        assert_eq!(cost.map(|(c, _)| c), Some(240_000));

        // Exact match: standard:1024x1024 → 40_000
        let usage = TokenUsage::for_images(1, Some("1024x1024"), Some("standard"));
        let cost = config.calculate_cost_detailed("openai", "dall-e-3", &usage);
        assert_eq!(cost.map(|(c, _)| c), Some(40_000));
    }

    #[test]
    fn test_image_pricing_table_wildcards() {
        let mut config = PricingConfig::default();
        let mut table = HashMap::new();
        table.insert("hd:1024x1024".to_string(), 80_000);
        table.insert("*:1024x1024".to_string(), 40_000); // quality wildcard
        table.insert("hd:*".to_string(), 100_000); // size wildcard
        table.insert("*:*".to_string(), 30_000); // full wildcard

        config.set_pricing(
            "openai",
            "dall-e-3",
            ModelPricing {
                per_image: Some(25_000),
                image_pricing: Some(table),
                ..Default::default()
            },
        );

        // Exact match takes priority
        let usage = TokenUsage::for_images(1, Some("1024x1024"), Some("hd"));
        let cost = config.calculate_cost_detailed("openai", "dall-e-3", &usage);
        assert_eq!(cost.map(|(c, _)| c), Some(80_000));

        // Quality wildcard: standard:1024x1024 → *:1024x1024
        let usage = TokenUsage::for_images(1, Some("1024x1024"), Some("standard"));
        let cost = config.calculate_cost_detailed("openai", "dall-e-3", &usage);
        assert_eq!(cost.map(|(c, _)| c), Some(40_000));

        // Size wildcard: hd:1792x1024 → hd:*
        let usage = TokenUsage::for_images(1, Some("1792x1024"), Some("hd"));
        let cost = config.calculate_cost_detailed("openai", "dall-e-3", &usage);
        assert_eq!(cost.map(|(c, _)| c), Some(100_000));

        // Full wildcard: standard:512x512 → *:*
        let usage = TokenUsage::for_images(1, Some("512x512"), Some("standard"));
        let cost = config.calculate_cost_detailed("openai", "dall-e-3", &usage);
        assert_eq!(cost.map(|(c, _)| c), Some(30_000));
    }

    #[test]
    fn test_image_pricing_table_fallback_to_per_image() {
        let mut config = PricingConfig::default();
        let mut table = HashMap::new();
        table.insert("hd:1024x1024".to_string(), 80_000);

        config.set_pricing(
            "openai",
            "dall-e-3",
            ModelPricing {
                per_image: Some(40_000),
                image_pricing: Some(table),
                ..Default::default()
            },
        );

        // No matching key → falls back to per_image
        let usage = TokenUsage::for_images(1, Some("512x512"), Some("standard"));
        let cost = config.calculate_cost_detailed("openai", "dall-e-3", &usage);
        assert_eq!(cost.map(|(c, _)| c), Some(40_000));
    }

    #[test]
    fn test_image_pricing_table_no_size_or_quality() {
        let mut config = PricingConfig::default();
        let mut table = HashMap::new();
        table.insert("*:*".to_string(), 50_000);
        table.insert("hd:1024x1024".to_string(), 80_000);

        config.set_pricing(
            "openai",
            "dall-e-3",
            ModelPricing {
                per_image: Some(40_000),
                image_pricing: Some(table),
                ..Default::default()
            },
        );

        // No size or quality → "*:*" match
        let usage = TokenUsage::for_images(1, None, None);
        let cost = config.calculate_cost_detailed("openai", "dall-e-3", &usage);
        assert_eq!(cost.map(|(c, _)| c), Some(50_000));
    }

    #[test]
    fn test_image_pricing_table_backwards_compat() {
        // per_image only, no image_pricing table — should work as before
        let mut config = PricingConfig::default();
        config.set_pricing(
            "openai",
            "dall-e-3",
            ModelPricing {
                per_image: Some(40_000),
                ..Default::default()
            },
        );

        let usage = TokenUsage::for_images(3, Some("1024x1024"), Some("hd"));
        let cost = config.calculate_cost_detailed("openai", "dall-e-3", &usage);
        assert_eq!(cost.map(|(c, _)| c), Some(120_000));
    }

    #[test]
    fn test_image_pricing_toml_deserialization() {
        let toml_str = r#"
            input_per_1m_tokens = 0
            output_per_1m_tokens = 0
            per_image = 40000

            [image_pricing]
            "standard:1024x1024" = 40000
            "hd:1024x1024" = 80000
            "hd:1792x1024" = 120000
        "#;

        let pricing: ModelPricing = toml::from_str(toml_str).unwrap();
        assert_eq!(pricing.per_image, Some(40_000));
        let table = pricing.image_pricing.as_ref().unwrap();
        assert_eq!(table.get("standard:1024x1024"), Some(&40_000));
        assert_eq!(table.get("hd:1024x1024"), Some(&80_000));
        assert_eq!(table.get("hd:1792x1024"), Some(&120_000));
    }

    #[test]
    fn test_image_pricing_from_image_pricing_table() {
        let mut table = HashMap::new();
        table.insert("*:256x256".to_string(), 16_000);
        table.insert("*:512x512".to_string(), 18_000);
        table.insert("*:1024x1024".to_string(), 20_000);

        let pricing = ModelPricing::from_image_pricing_table(table);
        assert_eq!(
            pricing.resolve_image_price(Some("standard"), Some("256x256")),
            Some(16_000)
        );
        assert_eq!(
            pricing.resolve_image_price(Some("standard"), Some("512x512")),
            Some(18_000)
        );
        assert_eq!(
            pricing.resolve_image_price(Some("standard"), Some("1024x1024")),
            Some(20_000)
        );
        // No match, no per_image fallback
        assert_eq!(
            pricing.resolve_image_price(Some("standard"), Some("2048x2048")),
            None
        );
    }

    #[test]
    fn test_audio_transcription_pricing() {
        let mut config = PricingConfig::default();

        // Whisper: $0.006/min = 6_000 microcents/min = 100 microcents/sec
        config.set_pricing(
            "openai",
            "whisper-1",
            ModelPricing::from_dollars_per_minute(0.006),
        );

        // 60 seconds of audio
        let usage = TokenUsage::for_audio_seconds(60);
        let cost = config.calculate_cost_detailed("openai", "whisper-1", &usage);
        assert_eq!(cost.map(|(c, _)| c), Some(6_000)); // $0.006

        // 5 minutes (300 seconds)
        let usage = TokenUsage::for_audio_seconds(300);
        let cost = config.calculate_cost_detailed("openai", "whisper-1", &usage);
        assert_eq!(cost.map(|(c, _)| c), Some(30_000)); // $0.03
    }

    #[test]
    fn test_tts_pricing() {
        let mut config = PricingConfig::default();

        // tts-1: $0.015/1K chars = $15/1M chars = 15_000_000 microcents/1M
        config.set_pricing(
            "openai",
            "tts-1",
            ModelPricing::from_dollars_per_1k_characters(0.015),
        );

        // 1000 characters = $0.015 = 15_000 microcents
        let usage = TokenUsage::for_tts_characters(1000);
        let cost = config.calculate_cost_detailed("openai", "tts-1", &usage);
        assert_eq!(cost.map(|(c, _)| c), Some(15_000)); // $0.015

        // 1 million characters = $15 = 15_000_000 microcents
        let usage = TokenUsage::for_tts_characters(1_000_000);
        let cost = config.calculate_cost_detailed("openai", "tts-1", &usage);
        assert_eq!(cost.map(|(c, _)| c), Some(15_000_000)); // $15

        // tts-1-hd: $0.030/1K chars
        config.set_pricing(
            "openai",
            "tts-1-hd",
            ModelPricing::from_dollars_per_1k_characters(0.030),
        );

        // 1 million characters = $30 = 30_000_000 microcents
        let usage = TokenUsage::for_tts_characters(1_000_000);
        let cost = config.calculate_cost_detailed("openai", "tts-1-hd", &usage);
        assert_eq!(cost.map(|(c, _)| c), Some(30_000_000)); // $30
    }

    #[test]
    fn test_from_dollars_per_image() {
        // $0.04/image = 40_000 microcents
        let pricing = ModelPricing::from_dollars_per_image(0.04);
        assert_eq!(pricing.per_image, Some(40_000));

        // $0.08/image (HD) = 80_000 microcents
        let pricing = ModelPricing::from_dollars_per_image(0.08);
        assert_eq!(pricing.per_image, Some(80_000));
    }

    #[test]
    fn test_from_dollars_per_minute() {
        // Whisper: $0.006/min = 100 microcents/sec
        let pricing = ModelPricing::from_dollars_per_minute(0.006);
        assert_eq!(pricing.per_second, Some(100));

        // $0.60/min = 10_000 microcents/sec
        let pricing = ModelPricing::from_dollars_per_minute(0.60);
        assert_eq!(pricing.per_second, Some(10_000));
    }

    #[test]
    fn test_from_dollars_per_1k_characters() {
        // tts-1: $0.015/1K = $15/1M = 15_000_000 microcents/1M
        let pricing = ModelPricing::from_dollars_per_1k_characters(0.015);
        assert_eq!(pricing.per_1m_characters, Some(15_000_000));

        // tts-1-hd: $0.030/1K = $30/1M = 30_000_000 microcents/1M
        let pricing = ModelPricing::from_dollars_per_1k_characters(0.030);
        assert_eq!(pricing.per_1m_characters, Some(30_000_000));
    }

    #[test]
    fn test_from_config_with_catalog() {
        use crate::{catalog::ModelCatalogRegistry, config::ProvidersConfig};

        // Create a minimal catalog with one model
        let catalog_json = r#"{
            "openai": {
                "id": "openai",
                "name": "OpenAI",
                "models": {
                    "gpt-4o": {
                        "id": "gpt-4o",
                        "name": "GPT-4o",
                        "cost": { "input": 2.5, "output": 10.0 }
                    },
                    "gpt-4o-mini": {
                        "id": "gpt-4o-mini",
                        "name": "GPT-4o Mini",
                        "cost": { "input": 0.15, "output": 0.6 }
                    }
                }
            }
        }"#;

        let catalog = ModelCatalogRegistry::new();
        catalog.load_from_json(catalog_json).unwrap();

        // Create provider config with allowed models
        let providers_toml = r#"
            [open_ai]
            type = "open_ai"
            api_key = "test-key"
            allowed_models = ["gpt-4o", "gpt-4o-mini"]
        "#;
        let providers: ProvidersConfig = toml::from_str(providers_toml).unwrap();

        // Create pricing config with override for one model
        let mut explicit_pricing = PricingConfig::default();
        explicit_pricing.set_pricing(
            "open_ai",
            "gpt-4o",
            ModelPricing {
                input_per_1m_tokens: 999_000, // Override catalog pricing
                output_per_1m_tokens: 999_000,
                ..Default::default()
            },
        );

        // Build final config with catalog fallback
        let result =
            PricingConfig::from_config_with_catalog(&explicit_pricing, &providers, Some(&catalog));

        // gpt-4o should use explicit override (higher priority)
        let gpt4o_pricing = result.get("open_ai", "gpt-4o").unwrap();
        assert_eq!(gpt4o_pricing.input_per_1m_tokens, 999_000);

        // gpt-4o-mini should use catalog pricing (fallback)
        let gpt4o_mini_pricing = result.get("open_ai", "gpt-4o-mini").unwrap();
        // $0.15 = 150_000 microcents
        assert_eq!(gpt4o_mini_pricing.input_per_1m_tokens, 150_000);
        // $0.6 = 600_000 microcents
        assert_eq!(gpt4o_mini_pricing.output_per_1m_tokens, 600_000);
    }

    #[test]
    fn test_catalog_fallback_for_unlisted_model() {
        use crate::{catalog::ModelCatalogRegistry, config::ProvidersConfig};

        let catalog_json = r#"{
            "openai": {
                "id": "openai",
                "name": "OpenAI",
                "models": {
                    "gpt-4o": {
                        "id": "gpt-4o",
                        "name": "GPT-4o",
                        "cost": { "input": 2.5, "output": 10.0 }
                    }
                }
            }
        }"#;

        let catalog = ModelCatalogRegistry::new();
        catalog.load_from_json(catalog_json).unwrap();

        // No allowed_models — passthrough mode
        let providers_toml = r#"
            [open_ai]
            type = "open_ai"
            api_key = "test-key"
        "#;
        let providers: ProvidersConfig = toml::from_str(providers_toml).unwrap();

        let result = PricingConfig::from_config_with_catalog(
            &PricingConfig::default(),
            &providers,
            Some(&catalog),
        );

        // gpt-4o is NOT in the HashMap (no allowed_models to pre-populate)
        assert!(result.get("open_ai", "gpt-4o").is_none());

        // But calculate_cost_detailed should find it via catalog fallback
        let cost = result.calculate_cost("open_ai", "gpt-4o", 1_000_000, 1_000_000);
        // $2.5/1M input + $10/1M output = $12.50 = 12_500_000 microcents
        assert_eq!(cost.map(|(c, _)| c), Some(12_500_000));
    }

    #[test]
    fn test_explicit_pricing_overrides_catalog() {
        use crate::{catalog::ModelCatalogRegistry, config::ProvidersConfig};

        let catalog_json = r#"{
            "openai": {
                "id": "openai",
                "name": "OpenAI",
                "models": {
                    "gpt-4o": {
                        "id": "gpt-4o",
                        "name": "GPT-4o",
                        "cost": { "input": 2.5, "output": 10.0 }
                    }
                }
            }
        }"#;

        let catalog = ModelCatalogRegistry::new();
        catalog.load_from_json(catalog_json).unwrap();

        let providers_toml = r#"
            [open_ai]
            type = "open_ai"
            api_key = "test-key"
        "#;
        let providers: ProvidersConfig = toml::from_str(providers_toml).unwrap();

        // Explicit pricing override
        let mut explicit = PricingConfig::default();
        explicit.set_pricing(
            "open_ai",
            "gpt-4o",
            ModelPricing {
                input_per_1m_tokens: 1_000,
                output_per_1m_tokens: 2_000,
                ..Default::default()
            },
        );

        let result = PricingConfig::from_config_with_catalog(&explicit, &providers, Some(&catalog));

        // HashMap has the explicit pricing, not catalog
        let cost = result.calculate_cost("open_ai", "gpt-4o", 1_000_000, 1_000_000);
        // 1_000 + 2_000 = 3_000 microcents
        assert_eq!(cost.map(|(c, _)| c), Some(3_000));
    }

    #[test]
    fn test_unknown_model_returns_none_with_catalog() {
        use crate::{catalog::ModelCatalogRegistry, config::ProvidersConfig};

        let catalog_json = r#"{
            "openai": {
                "id": "openai",
                "name": "OpenAI",
                "models": {
                    "gpt-4o": {
                        "id": "gpt-4o",
                        "name": "GPT-4o",
                        "cost": { "input": 2.5, "output": 10.0 }
                    }
                }
            }
        }"#;

        let catalog = ModelCatalogRegistry::new();
        catalog.load_from_json(catalog_json).unwrap();

        let providers_toml = r#"
            [open_ai]
            type = "open_ai"
            api_key = "test-key"
        "#;
        let providers: ProvidersConfig = toml::from_str(providers_toml).unwrap();

        let result = PricingConfig::from_config_with_catalog(
            &PricingConfig::default(),
            &providers,
            Some(&catalog),
        );

        // Model not in catalog or HashMap
        assert_eq!(
            result.calculate_cost("open_ai", "totally-fake-model", 1000, 1000),
            None
        );
    }

    #[test]
    fn test_no_catalog_configured() {
        // PricingConfig::default() has catalog: None — should behave as before
        let config = PricingConfig::default();
        assert_eq!(config.calculate_cost("openai", "gpt-4o", 1000, 1000), None);
    }
}
