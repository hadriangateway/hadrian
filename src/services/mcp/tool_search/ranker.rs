//! Ranking strategies for Hadrian-side tool search.
//!
//! A [`ToolSearchRanker`] scores a deferred MCP catalog against the
//! model's search query and returns the tools in descending relevance
//! order. The executor then applies the configured score threshold and
//! result cap. Three strategies ship:
//!
//! - [`LexicalRanker`] — token overlap with substring matching over the
//!   tool name, description, annotation title, and schema parameter
//!   names/descriptions. No dependency; always available. Returns only
//!   tools with non-zero overlap.
//! - [`SemanticRanker`] — embedding cosine similarity, reusing Hadrian's
//!   [`EmbeddingService`] and the [`McpService`] embedding cache. Returns
//!   every tool scored.
//! - [`HybridRanker`] — fuses the two with Reciprocal Rank Fusion (RRF),
//!   the default when an embedding provider is available.
//!
//! OpenAI's hosted tool search is semantic; mirroring that, `hybrid` /
//! `semantic` are preferred and `lexical` is the dependency-free
//! fallback when no embedding provider resolves.

use std::sync::Arc;

use async_trait::async_trait;

use super::super::{McpService, McpToolMeta};
use crate::cache::EmbeddingService;

/// A scored tool: the `index` into the catalog slice passed to
/// [`ToolSearchRanker::rank`], plus a relevance `score`. Lexical and
/// semantic scores are in `0.0..=1.0`; hybrid RRF scores are a smaller
/// ranking-only range (see [`HybridRanker`]).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RankedTool {
    pub index: usize,
    pub score: f64,
}

#[derive(Debug, thiserror::Error)]
pub enum RankError {
    #[error("embedding failed: {0}")]
    Embedding(String),
}

/// Ranks a deferred MCP catalog against a search query, best first.
#[async_trait]
pub trait ToolSearchRanker: Send + Sync {
    async fn rank(&self, query: &str, tools: &[McpToolMeta]) -> Result<Vec<RankedTool>, RankError>;

    /// Whether [`Self::rank`] returns scores normalized to `0.0..=1.0`,
    /// so the configured `score_threshold` is meaningful against them.
    /// Lexical and semantic scores are normalized; hybrid RRF scores are
    /// a ranking-only quantity (a tiny range like `0.0..≈0.033`) and
    /// return `false` so the executor skips the threshold rather than
    /// dropping every result. Defaults to `true`.
    fn scores_are_normalized(&self) -> bool {
        true
    }
}

/// Text representation of a tool used for both lexical and semantic
/// scoring: name, description, the annotation `title` if present, and
/// the input-schema parameter names + their descriptions. Indexing the
/// schema matters for tools whose discriminating keywords live in their
/// parameters (e.g. a generic `search` tool with a `repository` param)
/// rather than the name/description.
fn tool_text(meta: &McpToolMeta) -> String {
    let mut s = meta.name.clone();
    if let Some(d) = meta.description.as_deref().filter(|d| !d.is_empty()) {
        s.push('\n');
        s.push_str(d);
    }
    if let Some(title) = meta
        .annotations
        .as_ref()
        .and_then(|a| a.get("title"))
        .and_then(|v| v.as_str())
    {
        s.push('\n');
        s.push_str(title);
    }
    if let Some(props) = meta
        .input_schema
        .get("properties")
        .and_then(|p| p.as_object())
    {
        for (name, schema) in props {
            s.push('\n');
            s.push_str(name);
            if let Some(d) = schema.get("description").and_then(|v| v.as_str()) {
                s.push(' ');
                s.push_str(d);
            }
        }
    }
    s
}

/// Lowercase alphanumeric tokens.
fn tokenize(s: &str) -> Vec<String> {
    s.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .map(str::to_string)
        .collect()
}

/// Cosine similarity of two equal-length vectors. Returns 0 for a
/// zero-magnitude vector. Mirrors the RAG path's similarity measure.
fn cosine_similarity(a: &[f64], b: &[f64]) -> f64 {
    let dot: f64 = a.iter().zip(b.iter()).map(|(x, y)| x * y).sum();
    let mag_a = a.iter().map(|x| x * x).sum::<f64>().sqrt();
    let mag_b = b.iter().map(|x| x * x).sum::<f64>().sqrt();
    if mag_a == 0.0 || mag_b == 0.0 {
        0.0
    } else {
        dot / (mag_a * mag_b)
    }
}

/// Sort scored tools by descending score, breaking ties by original
/// index for deterministic output.
fn sort_ranked(mut ranked: Vec<RankedTool>) -> Vec<RankedTool> {
    ranked.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.index.cmp(&b.index))
    });
    ranked
}

/// True when query token `q` matches any token in `tokens` — exact, or
/// substring in either direction for tokens of length ≥ 3 (so "jira"
/// matches "jiras" and "searching" matches "search", without short
/// tokens like "id" matching everything).
fn token_matches(q: &str, tokens: &[String]) -> bool {
    tokens.iter().any(|t| {
        t.as_str() == q
            || (q.len() >= 3 && t.contains(q))
            || (t.len() >= 3 && q.contains(t.as_str()))
    })
}

/// Token-overlap ranking with substring matching. No embedding
/// dependency.
#[derive(Default)]
pub struct LexicalRanker;

impl LexicalRanker {
    fn score(query_tokens: &[String], meta: &McpToolMeta) -> f64 {
        if query_tokens.is_empty() {
            return 0.0;
        }
        let text = tool_text(meta);
        let tool_tokens = tokenize(&text);
        let matched = query_tokens
            .iter()
            .filter(|q| token_matches(q, &tool_tokens))
            .count();
        let overlap = matched as f64 / query_tokens.len() as f64;
        // Boost matches that land in the tool name — those are the
        // strongest signal that the model wants this exact tool. Blend
        // (rather than add-then-clamp) so the boost is never erased on a
        // high-overlap match and the result stays in `0.0..=1.0`: a
        // name-token hit always outranks a description-only match of
        // equal overlap.
        let name_tokens = tokenize(&meta.name);
        let name_hit = query_tokens.iter().any(|q| token_matches(q, &name_tokens));
        overlap * 0.75 + if name_hit { 0.25 } else { 0.0 }
    }
}

#[async_trait]
impl ToolSearchRanker for LexicalRanker {
    async fn rank(&self, query: &str, tools: &[McpToolMeta]) -> Result<Vec<RankedTool>, RankError> {
        let query_tokens = tokenize(query);
        let ranked: Vec<RankedTool> = tools
            .iter()
            .enumerate()
            .filter_map(|(index, meta)| {
                let score = Self::score(&query_tokens, meta);
                // Drop zero-overlap tools — they're noise, and keeping
                // them would let a default threshold of 0.0 surface the
                // whole catalog.
                (score > 0.0).then_some(RankedTool { index, score })
            })
            .collect();
        Ok(sort_ranked(ranked))
    }
}

/// Embedding cosine-similarity ranking. Reuses [`EmbeddingService`] and
/// caches tool-description embeddings on [`McpService`] so a static
/// catalog is embedded once, not per request.
pub struct SemanticRanker {
    embeddings: Arc<EmbeddingService>,
    service: McpService,
}

impl SemanticRanker {
    pub fn new(embeddings: Arc<EmbeddingService>, service: McpService) -> Self {
        Self {
            embeddings,
            service,
        }
    }

    /// Resolve an embedding per tool, hitting the cache first and
    /// embedding the misses in a single batch call.
    async fn tool_embeddings(&self, texts: &[String]) -> Result<Vec<Arc<Vec<f64>>>, RankError> {
        let model = self.embeddings.model();
        let mut resolved: Vec<Option<Arc<Vec<f64>>>> = texts
            .iter()
            .map(|t| self.service.cached_embedding(model, t))
            .collect();

        let miss_idx: Vec<usize> = resolved
            .iter()
            .enumerate()
            .filter_map(|(i, e)| e.is_none().then_some(i))
            .collect();

        if !miss_idx.is_empty() {
            let miss_texts: Vec<String> = miss_idx.iter().map(|&i| texts[i].clone()).collect();
            let embedded = self
                .embeddings
                .embed_batch(&miss_texts)
                .await
                .map_err(|e| RankError::Embedding(e.to_string()))?;
            // Don't index blindly: a provider that drops/dedups inputs
            // (e.g. empty strings) could return fewer vectors than we
            // asked for. Indexing `embedded[j]` would then panic.
            if embedded.len() != miss_texts.len() {
                return Err(RankError::Embedding(format!(
                    "embedding provider returned {} vectors for {} inputs",
                    embedded.len(),
                    miss_texts.len()
                )));
            }
            for (j, &i) in miss_idx.iter().enumerate() {
                let arc = Arc::new(embedded[j].clone());
                self.service.cache_embedding(model, &texts[i], arc.clone());
                resolved[i] = Some(arc);
            }
        }

        // Every slot is Some now (cache hit or freshly embedded).
        Ok(resolved.into_iter().map(|e| e.expect("filled")).collect())
    }
}

#[async_trait]
impl ToolSearchRanker for SemanticRanker {
    async fn rank(&self, query: &str, tools: &[McpToolMeta]) -> Result<Vec<RankedTool>, RankError> {
        if tools.is_empty() {
            return Ok(Vec::new());
        }
        let texts: Vec<String> = tools.iter().map(tool_text).collect();
        let tool_embeddings = self.tool_embeddings(&texts).await?;
        let query_embedding = self
            .embeddings
            .embed_text(query)
            .await
            .map_err(|e| RankError::Embedding(e.to_string()))?;

        let ranked: Vec<RankedTool> = tool_embeddings
            .iter()
            .enumerate()
            .map(|(index, emb)| RankedTool {
                index,
                score: cosine_similarity(&query_embedding, emb).max(0.0),
            })
            .collect();
        Ok(sort_ranked(ranked))
    }
}

/// Reciprocal Rank Fusion of [`SemanticRanker`] and [`LexicalRanker`].
///
/// Each ranker produces an ordered list; a tool's fused score is
/// `Σ 1 / (k + rank)` over the lists it appears in (1-indexed rank).
/// Robust to embedding noise and to tools with sparse descriptions. The
/// fused score is a ranking-only quantity (typically `0.0..≈0.033` at
/// `k = 60`), not a similarity — keep `score_threshold` at 0 for hybrid.
pub struct HybridRanker {
    semantic: SemanticRanker,
    lexical: LexicalRanker,
    rrf_k: u32,
}

impl HybridRanker {
    pub fn new(semantic: SemanticRanker, rrf_k: u32) -> Self {
        Self {
            semantic,
            lexical: LexicalRanker,
            rrf_k,
        }
    }
}

#[async_trait]
impl ToolSearchRanker for HybridRanker {
    async fn rank(&self, query: &str, tools: &[McpToolMeta]) -> Result<Vec<RankedTool>, RankError> {
        let semantic = self.semantic.rank(query, tools).await?;
        let lexical = self.lexical.rank(query, tools).await?;

        let k = self.rrf_k as f64;
        let mut scores = vec![0.0f64; tools.len()];
        let mut seen = vec![false; tools.len()];
        for list in [&semantic, &lexical] {
            for (rank0, rt) in list.iter().enumerate() {
                scores[rt.index] += 1.0 / (k + (rank0 + 1) as f64);
                seen[rt.index] = true;
            }
        }

        let ranked: Vec<RankedTool> = scores
            .into_iter()
            .enumerate()
            .filter_map(|(index, score)| seen[index].then_some(RankedTool { index, score }))
            .collect();
        Ok(sort_ranked(ranked))
    }

    /// RRF fuses *ranks*, not similarities; the fused score is a small
    /// ranking-only quantity, not a `0..=1` relevance. The executor must
    /// not apply `score_threshold` to it.
    fn scores_are_normalized(&self) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meta(name: &str, desc: &str) -> McpToolMeta {
        McpToolMeta {
            name: name.to_string(),
            description: (!desc.is_empty()).then(|| desc.to_string()),
            input_schema: serde_json::json!({"type": "object"}),
            annotations: None,
        }
    }

    fn test_embedding_service() -> Arc<EmbeddingService> {
        let cfg = crate::config::EmbeddingConfig {
            provider: "test".to_string(),
            model: "test-embed".to_string(),
            dimensions: 64,
        };
        let test_cfg: crate::config::TestProviderConfig =
            toml::from_str("").expect("default test provider config");
        let provider_cfg = crate::config::ProviderConfig::Test(test_cfg);
        Arc::new(
            EmbeddingService::new(
                &cfg,
                &provider_cfg,
                &crate::providers::CircuitBreakerRegistry::new(),
                reqwest::Client::new(),
            )
            .expect("embedding service"),
        )
    }

    fn catalog() -> Vec<McpToolMeta> {
        vec![
            meta("jira_search", "Search Jira issues by query"),
            meta(
                "confluence_create_page",
                "Create a new Confluence wiki page",
            ),
            meta("github_list_pull_requests", "List GitHub pull requests"),
        ]
    }

    #[tokio::test]
    async fn lexical_ranks_query_relevant_tool_first() {
        let tools = catalog();
        let ranked = LexicalRanker
            .rank("search jira issues", &tools)
            .await
            .unwrap();
        assert!(!ranked.is_empty());
        assert_eq!(tools[ranked[0].index].name, "jira_search");
    }

    #[tokio::test]
    async fn lexical_drops_zero_overlap_tools() {
        let tools = catalog();
        // No token overlaps any tool text.
        let ranked = LexicalRanker.rank("xyzzy", &tools).await.unwrap();
        assert!(ranked.is_empty());
    }

    #[tokio::test]
    async fn lexical_respects_top_ordering_by_score() {
        let tools = catalog();
        let ranked = LexicalRanker
            .rank("github pull requests", &tools)
            .await
            .unwrap();
        assert_eq!(tools[ranked[0].index].name, "github_list_pull_requests");
        // Scores are sorted descending.
        for w in ranked.windows(2) {
            assert!(w[0].score >= w[1].score);
        }
    }

    #[tokio::test]
    async fn semantic_ranks_query_relevant_tool_first() {
        // The Test provider's word-based embeddings give shared words
        // positive cosine similarity, so the Jira tool should win for a
        // Jira query.
        let svc = McpService::new();
        let ranker = SemanticRanker::new(test_embedding_service(), svc);
        let tools = catalog();
        let ranked = ranker.rank("search jira issues", &tools).await.unwrap();
        assert_eq!(ranked.len(), 3);
        assert_eq!(tools[ranked[0].index].name, "jira_search");
    }

    #[tokio::test]
    async fn semantic_uses_embedding_cache_on_second_call() {
        let svc = McpService::new();
        let ranker = SemanticRanker::new(test_embedding_service(), svc.clone());
        let tools = catalog();
        let _ = ranker.rank("search jira", &tools).await.unwrap();
        // Tool descriptions should now be cached (one entry per tool).
        for t in &tools {
            assert!(svc.cached_embedding("test-embed", &tool_text(t)).is_some());
        }
    }

    #[test]
    fn normalized_score_flag_matches_ranker_kind() {
        // Lexical/semantic scores are 0..=1 (threshold applies); hybrid
        // RRF scores are ranking-only (threshold must be skipped).
        assert!(LexicalRanker.scores_are_normalized());
        let svc = McpService::new();
        let semantic = SemanticRanker::new(test_embedding_service(), svc.clone());
        assert!(semantic.scores_are_normalized());
        let hybrid = HybridRanker::new(SemanticRanker::new(test_embedding_service(), svc), 60);
        assert!(!hybrid.scores_are_normalized());
    }

    #[tokio::test]
    async fn lexical_matches_query_against_schema_parameters() {
        // A tool whose discriminating keyword lives only in a parameter
        // name/description is still findable after indexing the schema.
        let tool = McpToolMeta {
            name: "search".to_string(),
            description: Some("Run a search".to_string()),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "repository": { "type": "string", "description": "GitHub repo slug" }
                }
            }),
            annotations: None,
        };
        let ranked = LexicalRanker.rank("repository", &[tool]).await.unwrap();
        assert_eq!(ranked.len(), 1);
    }

    #[tokio::test]
    async fn hybrid_fuses_both_rankers_and_covers_union() {
        let svc = McpService::new();
        let semantic = SemanticRanker::new(test_embedding_service(), svc);
        let ranker = HybridRanker::new(semantic, 60);
        let tools = catalog();
        let ranked = ranker.rank("search jira issues", &tools).await.unwrap();
        // Semantic scores all three, so the fused list covers the union.
        assert_eq!(ranked.len(), 3);
        assert_eq!(tools[ranked[0].index].name, "jira_search");
        for w in ranked.windows(2) {
            assert!(w[0].score >= w[1].score);
        }
    }
}
