//! Shared tests for UsageRepo implementations
//!
//! Tests are written as async functions that take a test context containing
//! the usage repo and utilities for creating test API keys.

use chrono::{Duration, Utc};
use uuid::Uuid;

use crate::{
    db::repos::{ApiKeyRepo, DateRange, OrganizationRepo, UsageRepo},
    models::{ApiKeyOwner, CreateApiKey, CreateOrganization, UsageLogEntry},
};

// ============================================================================
// Test Context
// ============================================================================

/// Test context containing repos needed for usage tests
pub struct UsageTestContext<'a> {
    pub usage_repo: &'a dyn UsageRepo,
    pub api_key_repo: &'a dyn ApiKeyRepo,
    pub org_repo: &'a dyn OrganizationRepo,
}

impl<'a> UsageTestContext<'a> {
    /// Create a test organization and return its ID
    pub async fn create_test_org(&self, slug: &str) -> Uuid {
        self.org_repo
            .create(CreateOrganization {
                slug: slug.to_string(),
                name: format!("Org {}", slug),
            })
            .await
            .expect("Failed to create test org")
            .id
    }

    /// Create a test API key and return its ID
    pub async fn create_test_api_key(&self, org_id: Uuid, name: &str) -> Uuid {
        let hash = format!("hash_{}", Uuid::new_v4().to_string().replace("-", ""));
        self.api_key_repo
            .create(
                CreateApiKey {
                    name: name.to_string(),
                    owner: ApiKeyOwner::Organization { org_id },
                    budget_limit_cents: None,
                    budget_period: None,
                    expires_at: None,
                    scopes: None,
                    allowed_models: None,
                    ip_allowlist: None,
                    rate_limit_rpm: None,
                    rate_limit_tpm: None,
                    sovereignty_requirements: None,
                },
                &hash,
            )
            .await
            .expect("Failed to create test API key")
            .id
    }
}

// ============================================================================
// Test Input Helpers
// ============================================================================

fn create_usage_entry(
    api_key_id: Uuid,
    model: &str,
    provider: &str,
    input_tokens: i32,
    output_tokens: i32,
    cost_microcents: Option<i64>,
) -> UsageLogEntry {
    UsageLogEntry {
        request_id: Uuid::new_v4().to_string(),
        client_request_id: None,
        api_key_id: Some(api_key_id),
        user_id: None,
        org_id: None,
        project_id: None,
        team_id: None,
        service_account_id: None,
        model: model.to_string(),
        provider: provider.to_string(),
        http_referer: None,
        input_tokens,
        output_tokens,
        cost_microcents,
        request_at: Utc::now(),
        streamed: false,
        cached_tokens: 0,
        reasoning_tokens: 0,
        finish_reason: None,
        latency_ms: None,
        cancelled: false,
        status_code: None,
        pricing_source: crate::pricing::CostPricingSource::None,
        image_count: None,
        audio_seconds: None,
        character_count: None,
        provider_source: None,
        record_type: "model".to_string(),
        tool_name: None,
        tool_query: None,
        tool_url: None,
        tool_bytes_fetched: None,
        tool_results_count: None,
        tool_runtime_seconds: None,
        tool_exit_code: None,
    }
}

/// A server-side tool usage entry (`record_type = "tool"`).
fn create_tool_usage_entry(api_key_id: Uuid, cost_microcents: i64) -> UsageLogEntry {
    UsageLogEntry {
        record_type: "tool".to_string(),
        tool_name: Some("web_search".to_string()),
        ..create_usage_entry(
            api_key_id,
            "web_search",
            "openai",
            0,
            0,
            Some(cost_microcents),
        )
    }
}

fn create_usage_entry_with_referer(
    api_key_id: Uuid,
    model: &str,
    http_referer: Option<&str>,
    cost_microcents: i64,
) -> UsageLogEntry {
    UsageLogEntry {
        request_id: Uuid::new_v4().to_string(),
        client_request_id: None,
        api_key_id: Some(api_key_id),
        user_id: None,
        org_id: None,
        project_id: None,
        team_id: None,
        service_account_id: None,
        model: model.to_string(),
        provider: "openai".to_string(),
        http_referer: http_referer.map(String::from),
        input_tokens: 100,
        output_tokens: 50,
        cost_microcents: Some(cost_microcents),
        request_at: Utc::now(),
        streamed: false,
        cached_tokens: 0,
        reasoning_tokens: 0,
        finish_reason: None,
        latency_ms: None,
        cancelled: false,
        status_code: None,
        pricing_source: crate::pricing::CostPricingSource::None,
        image_count: None,
        audio_seconds: None,
        character_count: None,
        provider_source: None,
        record_type: "model".to_string(),
        tool_name: None,
        tool_query: None,
        tool_url: None,
        tool_bytes_fetched: None,
        tool_results_count: None,
        tool_runtime_seconds: None,
        tool_exit_code: None,
    }
}

fn create_usage_entry_at_time(
    api_key_id: Uuid,
    model: &str,
    cost_microcents: i64,
    request_at: chrono::DateTime<Utc>,
) -> UsageLogEntry {
    UsageLogEntry {
        request_id: Uuid::new_v4().to_string(),
        client_request_id: None,
        api_key_id: Some(api_key_id),
        user_id: None,
        org_id: None,
        project_id: None,
        team_id: None,
        service_account_id: None,
        model: model.to_string(),
        provider: "openai".to_string(),
        http_referer: None,
        input_tokens: 100,
        output_tokens: 50,
        cost_microcents: Some(cost_microcents),
        request_at,
        streamed: false,
        cached_tokens: 0,
        reasoning_tokens: 0,
        finish_reason: None,
        latency_ms: None,
        cancelled: false,
        status_code: None,
        pricing_source: crate::pricing::CostPricingSource::None,
        image_count: None,
        audio_seconds: None,
        character_count: None,
        provider_source: None,
        record_type: "model".to_string(),
        tool_name: None,
        tool_query: None,
        tool_url: None,
        tool_bytes_fetched: None,
        tool_results_count: None,
        tool_runtime_seconds: None,
        tool_exit_code: None,
    }
}

#[derive(Default)]
struct UsageAttribution {
    api_key_id: Option<Uuid>,
    user_id: Option<Uuid>,
    org_id: Option<Uuid>,
    project_id: Option<Uuid>,
    team_id: Option<Uuid>,
}

/// Create a usage entry with full attribution context (for session-user tests)
fn create_attributed_usage_entry(
    attr: UsageAttribution,
    model: &str,
    provider: &str,
    cost_microcents: i64,
) -> UsageLogEntry {
    UsageLogEntry {
        request_id: Uuid::new_v4().to_string(),
        client_request_id: None,
        api_key_id: attr.api_key_id,
        user_id: attr.user_id,
        org_id: attr.org_id,
        project_id: attr.project_id,
        team_id: attr.team_id,
        service_account_id: None,
        model: model.to_string(),
        provider: provider.to_string(),
        http_referer: None,
        input_tokens: 100,
        output_tokens: 50,
        cost_microcents: Some(cost_microcents),
        request_at: Utc::now(),
        streamed: false,
        cached_tokens: 0,
        reasoning_tokens: 0,
        finish_reason: None,
        latency_ms: None,
        cancelled: false,
        status_code: None,
        pricing_source: crate::pricing::CostPricingSource::None,
        image_count: None,
        audio_seconds: None,
        character_count: None,
        provider_source: None,
        record_type: "model".to_string(),
        tool_name: None,
        tool_query: None,
        tool_url: None,
        tool_bytes_fetched: None,
        tool_results_count: None,
        tool_runtime_seconds: None,
        tool_exit_code: None,
    }
}

fn today_range() -> DateRange {
    let today = Utc::now().date_naive();
    DateRange {
        start: today,
        end: today,
        record_type: None,
    }
}

/// Today's range restricted to a single record type ("model" or "tool").
fn today_range_of(record_type: &str) -> DateRange {
    DateRange {
        record_type: Some(record_type.to_string()),
        ..today_range()
    }
}

fn date_range(start: chrono::NaiveDate, end: chrono::NaiveDate) -> DateRange {
    DateRange {
        start,
        end,
        record_type: None,
    }
}

// ============================================================================
// Log Tests
// ============================================================================

pub async fn test_log_basic(ctx: &UsageTestContext<'_>) {
    let org_id = ctx.create_test_org("test-org").await;
    let api_key_id = ctx.create_test_api_key(org_id, "test-key").await;

    let entry = create_usage_entry(api_key_id, "gpt-4", "openai", 100, 50, Some(500));
    ctx.usage_repo
        .log(entry)
        .await
        .expect("Failed to log usage entry");

    // Verify via get_summary
    let summary = ctx
        .usage_repo
        .get_summary(api_key_id, today_range())
        .await
        .expect("Failed to get summary");

    assert_eq!(summary.request_count, 1);
}

pub async fn test_log_calculates_total_tokens(ctx: &UsageTestContext<'_>) {
    let org_id = ctx.create_test_org("test-org").await;
    let api_key_id = ctx.create_test_api_key(org_id, "test-key").await;

    let entry = create_usage_entry(api_key_id, "gpt-4", "openai", 100, 50, Some(500));
    ctx.usage_repo
        .log(entry)
        .await
        .expect("Failed to log usage entry");

    // Verify via get_summary - total_tokens should be 150 (100 + 50)
    let summary = ctx
        .usage_repo
        .get_summary(api_key_id, today_range())
        .await
        .expect("Failed to get summary");

    assert_eq!(summary.total_tokens, 150);
}

pub async fn test_log_with_no_cost(ctx: &UsageTestContext<'_>) {
    let org_id = ctx.create_test_org("test-org").await;
    let api_key_id = ctx.create_test_api_key(org_id, "test-key").await;

    let entry = create_usage_entry(api_key_id, "gpt-4", "openai", 100, 50, None);
    ctx.usage_repo
        .log(entry)
        .await
        .expect("Failed to log usage entry");

    // Verify cost defaults to 0
    let summary = ctx
        .usage_repo
        .get_summary(api_key_id, today_range())
        .await
        .expect("Failed to get summary");

    assert_eq!(summary.total_cost_microcents, 0);
}

pub async fn test_log_with_referer(ctx: &UsageTestContext<'_>) {
    let org_id = ctx.create_test_org("test-org").await;
    let api_key_id = ctx.create_test_api_key(org_id, "test-key").await;

    let entry =
        create_usage_entry_with_referer(api_key_id, "gpt-4", Some("https://example.com"), 500);
    ctx.usage_repo
        .log(entry)
        .await
        .expect("Failed to log usage entry");

    // Verify referer via get_by_referer
    let result = ctx
        .usage_repo
        .get_by_referer(api_key_id, today_range())
        .await
        .expect("Failed to get by referer");

    assert_eq!(result.len(), 1);
    assert_eq!(result[0].referer, Some("https://example.com".to_string()));
}

pub async fn test_log_multiple_entries(ctx: &UsageTestContext<'_>) {
    let org_id = ctx.create_test_org("test-org").await;
    let api_key_id = ctx.create_test_api_key(org_id, "test-key").await;

    for i in 0..5 {
        let entry = create_usage_entry(api_key_id, "gpt-4", "openai", 100 * i, 50 * i, Some(500));
        ctx.usage_repo
            .log(entry)
            .await
            .expect("Failed to log usage entry");
    }

    let summary = ctx
        .usage_repo
        .get_summary(api_key_id, today_range())
        .await
        .expect("Failed to get summary");

    assert_eq!(summary.request_count, 5);
}

// ============================================================================
// Log Batch Tests
// ============================================================================

/// Round-trip the new `tool_runtime_seconds` field through both the
/// single-insert `log` path and `list_logs`, so a positional bug in
/// the 35-parameter INSERT (or a missing column in the row reader)
/// can't silently null the field.
pub async fn test_log_tool_runtime_seconds_round_trip(ctx: &UsageTestContext<'_>) {
    use crate::db::repos::UsageLogQuery;
    let org_id = ctx.create_test_org("test-org-toolrt").await;
    let api_key_id = ctx.create_test_api_key(org_id, "test-key-toolrt").await;

    let mut entry = create_usage_entry(api_key_id, "gpt-4", "openai", 0, 0, Some(0));
    entry.record_type = "tool".to_string();
    entry.tool_name = Some("shell".to_string());
    entry.tool_runtime_seconds = Some(12.5);

    ctx.usage_repo
        .log(entry)
        .await
        .expect("Failed to log tool usage entry");

    let listed = ctx
        .usage_repo
        .list_logs(UsageLogQuery {
            api_key_id: Some(api_key_id),
            record_type: Some("tool".to_string()),
            limit: Some(10),
            ..Default::default()
        })
        .await
        .expect("Failed to list logs");

    assert_eq!(listed.items.len(), 1, "expected one tool record");
    let record = &listed.items[0];
    assert_eq!(record.tool_name.as_deref(), Some("shell"));
    let runtime = record
        .tool_runtime_seconds
        .expect("tool_runtime_seconds should round-trip as Some(_)");
    assert!(
        (runtime - 12.5).abs() < 1e-9,
        "tool_runtime_seconds {runtime} did not match 12.5"
    );
}

pub async fn test_log_batch_empty(ctx: &UsageTestContext<'_>) {
    // Empty batch should return 0 without error
    let result = ctx
        .usage_repo
        .log_batch(vec![])
        .await
        .expect("Failed to log empty batch");

    assert_eq!(result, 0);
}

pub async fn test_log_batch_basic(ctx: &UsageTestContext<'_>) {
    let org_id = ctx.create_test_org("test-org").await;
    let api_key_id = ctx.create_test_api_key(org_id, "test-key").await;

    let entries: Vec<UsageLogEntry> = (0..5)
        .map(|i| create_usage_entry(api_key_id, "gpt-4", "openai", 100 * i, 50 * i, Some(500)))
        .collect();

    let inserted = ctx
        .usage_repo
        .log_batch(entries)
        .await
        .expect("Failed to log batch");

    assert_eq!(inserted, 5);

    // Verify via get_summary
    let summary = ctx
        .usage_repo
        .get_summary(api_key_id, today_range())
        .await
        .expect("Failed to get summary");

    assert_eq!(summary.request_count, 5);
    assert_eq!(summary.total_cost_microcents, 2500); // 500 * 5
}

pub async fn test_log_batch_large_batch_spans_multiple_chunks(ctx: &UsageTestContext<'_>) {
    let org_id = ctx.create_test_org("test-org").await;
    let api_key_id = ctx.create_test_api_key(org_id, "test-key").await;

    // Create a batch larger than SQLite's MAX_ENTRIES_PER_BATCH (55)
    // This tests that the transaction wraps all chunks
    let entries: Vec<UsageLogEntry> = (0..100)
        .map(|i| create_usage_entry(api_key_id, "gpt-4", "openai", 100, 50, Some(i as i64)))
        .collect();

    let inserted = ctx
        .usage_repo
        .log_batch(entries)
        .await
        .expect("Failed to log large batch");

    assert_eq!(inserted, 100);

    // Verify all entries were inserted
    let summary = ctx
        .usage_repo
        .get_summary(api_key_id, today_range())
        .await
        .expect("Failed to get summary");

    assert_eq!(summary.request_count, 100);
    // Sum of 0..100 = 4950
    assert_eq!(summary.total_cost_microcents, 4950);
}

pub async fn test_log_batch_ignores_duplicates(ctx: &UsageTestContext<'_>) {
    let org_id = ctx.create_test_org("test-org").await;
    let api_key_id = ctx.create_test_api_key(org_id, "test-key").await;

    // Create entry with a fixed request_id
    let mut entry = create_usage_entry(api_key_id, "gpt-4", "openai", 100, 50, Some(500));
    let fixed_request_id = "fixed-request-id-12345".to_string();
    entry.request_id = fixed_request_id.clone();

    // Insert first time
    let inserted = ctx
        .usage_repo
        .log_batch(vec![entry.clone()])
        .await
        .expect("Failed to log batch");

    assert_eq!(inserted, 1);

    // Insert same entry again - should be ignored due to ON CONFLICT DO NOTHING
    let inserted_again = ctx
        .usage_repo
        .log_batch(vec![entry])
        .await
        .expect("Failed to log batch again");

    // Due to ON CONFLICT DO NOTHING, 0 rows are affected for duplicates
    assert_eq!(inserted_again, 0);

    // Verify only 1 entry exists
    let summary = ctx
        .usage_repo
        .get_summary(api_key_id, today_range())
        .await
        .expect("Failed to get summary");

    assert_eq!(summary.request_count, 1);
}

pub async fn test_log_batch_mixed_new_and_duplicate(ctx: &UsageTestContext<'_>) {
    let org_id = ctx.create_test_org("test-org").await;
    let api_key_id = ctx.create_test_api_key(org_id, "test-key").await;

    // Insert first entry
    let mut first_entry = create_usage_entry(api_key_id, "gpt-4", "openai", 100, 50, Some(500));
    first_entry.request_id = "duplicate-id".to_string();

    ctx.usage_repo
        .log_batch(vec![first_entry.clone()])
        .await
        .expect("Failed to log first batch");

    // Insert batch with one duplicate and two new entries
    let mut duplicate = first_entry.clone();
    duplicate.cost_microcents = Some(9999); // Different cost but same request_id

    let new_entry1 = create_usage_entry(api_key_id, "gpt-4", "openai", 200, 100, Some(1000));
    let new_entry2 = create_usage_entry(api_key_id, "gpt-4", "openai", 300, 150, Some(1500));

    let inserted = ctx
        .usage_repo
        .log_batch(vec![duplicate, new_entry1, new_entry2])
        .await
        .expect("Failed to log mixed batch");

    // Only 2 new entries inserted, duplicate was ignored
    assert_eq!(inserted, 2);

    // Verify total: 1 (first) + 2 (new) = 3 entries
    let summary = ctx
        .usage_repo
        .get_summary(api_key_id, today_range())
        .await
        .expect("Failed to get summary");

    assert_eq!(summary.request_count, 3);
    // 500 (first) + 1000 + 1500 = 3000 (not 9999 from duplicate)
    assert_eq!(summary.total_cost_microcents, 3000);
}

// ============================================================================
// Get Summary Tests
// ============================================================================

pub async fn test_get_summary_empty(ctx: &UsageTestContext<'_>) {
    let org_id = ctx.create_test_org("test-org").await;
    let api_key_id = ctx.create_test_api_key(org_id, "test-key").await;

    let summary = ctx
        .usage_repo
        .get_summary(api_key_id, today_range())
        .await
        .expect("Failed to get summary");

    assert_eq!(summary.total_cost_microcents, 0);
    assert_eq!(summary.total_tokens, 0);
    assert_eq!(summary.request_count, 0);
    assert!(summary.first_request_at.is_none());
    assert!(summary.last_request_at.is_none());
}

pub async fn test_get_summary_with_records(ctx: &UsageTestContext<'_>) {
    let org_id = ctx.create_test_org("test-org").await;
    let api_key_id = ctx.create_test_api_key(org_id, "test-key").await;

    // Log 3 entries
    for _ in 0..3 {
        let entry = create_usage_entry(api_key_id, "gpt-4", "openai", 100, 50, Some(500));
        ctx.usage_repo
            .log(entry)
            .await
            .expect("Failed to log usage entry");
    }

    let summary = ctx
        .usage_repo
        .get_summary(api_key_id, today_range())
        .await
        .expect("Failed to get summary");

    assert_eq!(summary.total_cost_microcents, 1500); // 500 * 3
    assert_eq!(summary.total_tokens, 450); // 150 * 3
    assert_eq!(summary.request_count, 3);
    assert!(summary.first_request_at.is_some());
    assert!(summary.last_request_at.is_some());
}

pub async fn test_get_summary_filters_by_api_key(ctx: &UsageTestContext<'_>) {
    let org_id = ctx.create_test_org("test-org").await;
    let api_key_1 = ctx.create_test_api_key(org_id, "key-1").await;
    let api_key_2 = ctx.create_test_api_key(org_id, "key-2").await;

    // Log 2 entries for key 1
    for _ in 0..2 {
        let entry = create_usage_entry(api_key_1, "gpt-4", "openai", 100, 50, Some(500));
        ctx.usage_repo
            .log(entry)
            .await
            .expect("Failed to log usage entry");
    }

    // Log 1 entry for key 2
    let entry = create_usage_entry(api_key_2, "gpt-4", "openai", 100, 50, Some(1000));
    ctx.usage_repo
        .log(entry)
        .await
        .expect("Failed to log usage entry");

    let summary_1 = ctx
        .usage_repo
        .get_summary(api_key_1, today_range())
        .await
        .expect("Failed to get summary");

    let summary_2 = ctx
        .usage_repo
        .get_summary(api_key_2, today_range())
        .await
        .expect("Failed to get summary");

    assert_eq!(summary_1.request_count, 2);
    assert_eq!(summary_1.total_cost_microcents, 1000);

    assert_eq!(summary_2.request_count, 1);
    assert_eq!(summary_2.total_cost_microcents, 1000);
}

pub async fn test_get_summary_filters_by_date_range(ctx: &UsageTestContext<'_>) {
    let org_id = ctx.create_test_org("test-org").await;
    let api_key_id = ctx.create_test_api_key(org_id, "test-key").await;

    let today = Utc::now();
    let yesterday = today - Duration::days(1);
    let two_days_ago = today - Duration::days(2);

    // Log entries at different times
    ctx.usage_repo
        .log(create_usage_entry_at_time(api_key_id, "gpt-4", 100, today))
        .await
        .expect("Failed to log");
    ctx.usage_repo
        .log(create_usage_entry_at_time(
            api_key_id, "gpt-4", 200, yesterday,
        ))
        .await
        .expect("Failed to log");
    ctx.usage_repo
        .log(create_usage_entry_at_time(
            api_key_id,
            "gpt-4",
            300,
            two_days_ago,
        ))
        .await
        .expect("Failed to log");

    // Query for just today
    let today_only = date_range(today.date_naive(), today.date_naive());
    let summary = ctx
        .usage_repo
        .get_summary(api_key_id, today_only)
        .await
        .expect("Failed to get summary");

    assert_eq!(summary.request_count, 1);
    assert_eq!(summary.total_cost_microcents, 100);

    // Query for last two days
    let two_days = date_range(yesterday.date_naive(), today.date_naive());
    let summary = ctx
        .usage_repo
        .get_summary(api_key_id, two_days)
        .await
        .expect("Failed to get summary");

    assert_eq!(summary.request_count, 2);
    assert_eq!(summary.total_cost_microcents, 300); // 100 + 200
}

// ============================================================================
// Get By Date Tests
// ============================================================================

pub async fn test_get_by_date_empty(ctx: &UsageTestContext<'_>) {
    let org_id = ctx.create_test_org("test-org").await;
    let api_key_id = ctx.create_test_api_key(org_id, "test-key").await;

    let result = ctx
        .usage_repo
        .get_by_date(api_key_id, today_range())
        .await
        .expect("Failed to get by date");

    assert!(result.is_empty());
}

pub async fn test_get_by_date_groups_by_date(ctx: &UsageTestContext<'_>) {
    let org_id = ctx.create_test_org("test-org").await;
    let api_key_id = ctx.create_test_api_key(org_id, "test-key").await;

    let today = Utc::now();
    let yesterday = today - Duration::days(1);

    // Log 2 entries today
    ctx.usage_repo
        .log(create_usage_entry_at_time(api_key_id, "gpt-4", 100, today))
        .await
        .expect("Failed to log");
    ctx.usage_repo
        .log(create_usage_entry_at_time(api_key_id, "gpt-4", 200, today))
        .await
        .expect("Failed to log");

    // Log 1 entry yesterday
    ctx.usage_repo
        .log(create_usage_entry_at_time(
            api_key_id, "gpt-4", 300, yesterday,
        ))
        .await
        .expect("Failed to log");

    let range = date_range(yesterday.date_naive(), today.date_naive());
    let result = ctx
        .usage_repo
        .get_by_date(api_key_id, range)
        .await
        .expect("Failed to get by date");

    assert_eq!(result.len(), 2);

    // Results are ordered by date DESC
    let today_spend = result
        .iter()
        .find(|d| d.date == today.date_naive())
        .unwrap();
    let yesterday_spend = result
        .iter()
        .find(|d| d.date == yesterday.date_naive())
        .unwrap();

    assert_eq!(today_spend.total_cost_microcents, 300); // 100 + 200
    assert_eq!(today_spend.request_count, 2);

    assert_eq!(yesterday_spend.total_cost_microcents, 300);
    assert_eq!(yesterday_spend.request_count, 1);
}

pub async fn test_get_by_date_filters_by_api_key(ctx: &UsageTestContext<'_>) {
    let org_id = ctx.create_test_org("test-org").await;
    let api_key_1 = ctx.create_test_api_key(org_id, "key-1").await;
    let api_key_2 = ctx.create_test_api_key(org_id, "key-2").await;

    ctx.usage_repo
        .log(create_usage_entry(
            api_key_1,
            "gpt-4",
            "openai",
            100,
            50,
            Some(500),
        ))
        .await
        .expect("Failed to log");
    ctx.usage_repo
        .log(create_usage_entry(
            api_key_2,
            "gpt-4",
            "openai",
            100,
            50,
            Some(1000),
        ))
        .await
        .expect("Failed to log");

    let result = ctx
        .usage_repo
        .get_by_date(api_key_1, today_range())
        .await
        .expect("Failed to get by date");

    assert_eq!(result.len(), 1);
    assert_eq!(result[0].total_cost_microcents, 500);
}

// ============================================================================
// Get By Model Tests
// ============================================================================

pub async fn test_get_by_model_empty(ctx: &UsageTestContext<'_>) {
    let org_id = ctx.create_test_org("test-org").await;
    let api_key_id = ctx.create_test_api_key(org_id, "test-key").await;

    let result = ctx
        .usage_repo
        .get_by_model(api_key_id, today_range())
        .await
        .expect("Failed to get by model");

    assert!(result.is_empty());
}

pub async fn test_get_by_model_groups_by_model(ctx: &UsageTestContext<'_>) {
    let org_id = ctx.create_test_org("test-org").await;
    let api_key_id = ctx.create_test_api_key(org_id, "test-key").await;

    // Log entries for different models
    ctx.usage_repo
        .log(create_usage_entry(
            api_key_id,
            "gpt-4",
            "openai",
            100,
            50,
            Some(500),
        ))
        .await
        .expect("Failed to log");
    ctx.usage_repo
        .log(create_usage_entry(
            api_key_id,
            "gpt-4",
            "openai",
            100,
            50,
            Some(500),
        ))
        .await
        .expect("Failed to log");
    ctx.usage_repo
        .log(create_usage_entry(
            api_key_id,
            "gpt-3.5-turbo",
            "openai",
            100,
            50,
            Some(100),
        ))
        .await
        .expect("Failed to log");
    ctx.usage_repo
        .log(create_usage_entry(
            api_key_id,
            "claude-3-opus",
            "anthropic",
            100,
            50,
            Some(1000),
        ))
        .await
        .expect("Failed to log");

    let result = ctx
        .usage_repo
        .get_by_model(api_key_id, today_range())
        .await
        .expect("Failed to get by model");

    assert_eq!(result.len(), 3);

    let gpt4 = result.iter().find(|m| m.model == "gpt-4").unwrap();
    let gpt35 = result.iter().find(|m| m.model == "gpt-3.5-turbo").unwrap();
    let claude = result.iter().find(|m| m.model == "claude-3-opus").unwrap();

    assert_eq!(gpt4.total_cost_microcents, 1000); // 500 + 500
    assert_eq!(gpt4.request_count, 2);

    assert_eq!(gpt35.total_cost_microcents, 100);
    assert_eq!(gpt35.request_count, 1);

    assert_eq!(claude.total_cost_microcents, 1000);
    assert_eq!(claude.request_count, 1);
}

pub async fn test_get_by_model_ordered_by_cost_desc(ctx: &UsageTestContext<'_>) {
    let org_id = ctx.create_test_org("test-org").await;
    let api_key_id = ctx.create_test_api_key(org_id, "test-key").await;

    ctx.usage_repo
        .log(create_usage_entry(
            api_key_id,
            "cheap-model",
            "openai",
            100,
            50,
            Some(100),
        ))
        .await
        .expect("Failed to log");
    ctx.usage_repo
        .log(create_usage_entry(
            api_key_id,
            "expensive-model",
            "openai",
            100,
            50,
            Some(10000),
        ))
        .await
        .expect("Failed to log");
    ctx.usage_repo
        .log(create_usage_entry(
            api_key_id,
            "medium-model",
            "openai",
            100,
            50,
            Some(1000),
        ))
        .await
        .expect("Failed to log");

    let result = ctx
        .usage_repo
        .get_by_model(api_key_id, today_range())
        .await
        .expect("Failed to get by model");

    assert_eq!(result.len(), 3);
    assert_eq!(result[0].model, "expensive-model");
    assert_eq!(result[1].model, "medium-model");
    assert_eq!(result[2].model, "cheap-model");
}

pub async fn test_get_by_model_filters_by_date_range(ctx: &UsageTestContext<'_>) {
    let org_id = ctx.create_test_org("test-org").await;
    let api_key_id = ctx.create_test_api_key(org_id, "test-key").await;

    let today = Utc::now();
    let yesterday = today - Duration::days(1);

    ctx.usage_repo
        .log(create_usage_entry_at_time(api_key_id, "gpt-4", 500, today))
        .await
        .expect("Failed to log");
    ctx.usage_repo
        .log(create_usage_entry_at_time(
            api_key_id, "gpt-4", 1000, yesterday,
        ))
        .await
        .expect("Failed to log");

    // Query for just today
    let result = ctx
        .usage_repo
        .get_by_model(
            api_key_id,
            date_range(today.date_naive(), today.date_naive()),
        )
        .await
        .expect("Failed to get by model");

    assert_eq!(result.len(), 1);
    assert_eq!(result[0].total_cost_microcents, 500);
}

// ============================================================================
// Get By Referer Tests
// ============================================================================

pub async fn test_get_by_referer_empty(ctx: &UsageTestContext<'_>) {
    let org_id = ctx.create_test_org("test-org").await;
    let api_key_id = ctx.create_test_api_key(org_id, "test-key").await;

    let result = ctx
        .usage_repo
        .get_by_referer(api_key_id, today_range())
        .await
        .expect("Failed to get by referer");

    assert!(result.is_empty());
}

pub async fn test_get_by_referer_groups_by_referer(ctx: &UsageTestContext<'_>) {
    let org_id = ctx.create_test_org("test-org").await;
    let api_key_id = ctx.create_test_api_key(org_id, "test-key").await;

    // Log entries from different referers
    ctx.usage_repo
        .log(create_usage_entry_with_referer(
            api_key_id,
            "gpt-4",
            Some("https://app.example.com"),
            500,
        ))
        .await
        .expect("Failed to log");
    ctx.usage_repo
        .log(create_usage_entry_with_referer(
            api_key_id,
            "gpt-4",
            Some("https://app.example.com"),
            500,
        ))
        .await
        .expect("Failed to log");
    ctx.usage_repo
        .log(create_usage_entry_with_referer(
            api_key_id,
            "gpt-4",
            Some("https://other.example.com"),
            1000,
        ))
        .await
        .expect("Failed to log");

    let result = ctx
        .usage_repo
        .get_by_referer(api_key_id, today_range())
        .await
        .expect("Failed to get by referer");

    assert_eq!(result.len(), 2);

    let app = result
        .iter()
        .find(|r| r.referer == Some("https://app.example.com".to_string()))
        .unwrap();
    let other = result
        .iter()
        .find(|r| r.referer == Some("https://other.example.com".to_string()))
        .unwrap();

    assert_eq!(app.total_cost_microcents, 1000); // 500 + 500
    assert_eq!(app.request_count, 2);

    assert_eq!(other.total_cost_microcents, 1000);
    assert_eq!(other.request_count, 1);
}

pub async fn test_get_by_referer_handles_null_referer(ctx: &UsageTestContext<'_>) {
    let org_id = ctx.create_test_org("test-org").await;
    let api_key_id = ctx.create_test_api_key(org_id, "test-key").await;

    // Log entries with and without referer
    ctx.usage_repo
        .log(create_usage_entry_with_referer(
            api_key_id,
            "gpt-4",
            Some("https://example.com"),
            500,
        ))
        .await
        .expect("Failed to log");
    ctx.usage_repo
        .log(create_usage_entry_with_referer(
            api_key_id, "gpt-4", None, 1000,
        ))
        .await
        .expect("Failed to log");
    ctx.usage_repo
        .log(create_usage_entry_with_referer(
            api_key_id, "gpt-4", None, 500,
        ))
        .await
        .expect("Failed to log");

    let result = ctx
        .usage_repo
        .get_by_referer(api_key_id, today_range())
        .await
        .expect("Failed to get by referer");

    assert_eq!(result.len(), 2);

    let with_referer = result.iter().find(|r| r.referer.is_some()).unwrap();
    let no_referer = result.iter().find(|r| r.referer.is_none()).unwrap();

    assert_eq!(with_referer.total_cost_microcents, 500);
    assert_eq!(no_referer.total_cost_microcents, 1500); // 1000 + 500
    assert_eq!(no_referer.request_count, 2);
}

pub async fn test_get_by_referer_ordered_by_cost_desc(ctx: &UsageTestContext<'_>) {
    let org_id = ctx.create_test_org("test-org").await;
    let api_key_id = ctx.create_test_api_key(org_id, "test-key").await;

    ctx.usage_repo
        .log(create_usage_entry_with_referer(
            api_key_id,
            "gpt-4",
            Some("https://cheap.com"),
            100,
        ))
        .await
        .expect("Failed to log");
    ctx.usage_repo
        .log(create_usage_entry_with_referer(
            api_key_id,
            "gpt-4",
            Some("https://expensive.com"),
            10000,
        ))
        .await
        .expect("Failed to log");
    ctx.usage_repo
        .log(create_usage_entry_with_referer(
            api_key_id,
            "gpt-4",
            Some("https://medium.com"),
            1000,
        ))
        .await
        .expect("Failed to log");

    let result = ctx
        .usage_repo
        .get_by_referer(api_key_id, today_range())
        .await
        .expect("Failed to get by referer");

    assert_eq!(result.len(), 3);
    assert_eq!(result[0].referer, Some("https://expensive.com".to_string()));
    assert_eq!(result[1].referer, Some("https://medium.com".to_string()));
    assert_eq!(result[2].referer, Some("https://cheap.com".to_string()));
}

pub async fn test_get_by_referer_filters_by_api_key(ctx: &UsageTestContext<'_>) {
    let org_id = ctx.create_test_org("test-org").await;
    let api_key_1 = ctx.create_test_api_key(org_id, "key-1").await;
    let api_key_2 = ctx.create_test_api_key(org_id, "key-2").await;

    ctx.usage_repo
        .log(create_usage_entry_with_referer(
            api_key_1,
            "gpt-4",
            Some("https://example.com"),
            500,
        ))
        .await
        .expect("Failed to log");
    ctx.usage_repo
        .log(create_usage_entry_with_referer(
            api_key_2,
            "gpt-4",
            Some("https://other.com"),
            1000,
        ))
        .await
        .expect("Failed to log");

    let result = ctx
        .usage_repo
        .get_by_referer(api_key_1, today_range())
        .await
        .expect("Failed to get by referer");

    assert_eq!(result.len(), 1);
    assert_eq!(result[0].referer, Some("https://example.com".to_string()));
}

// ============================================================================
// Get Usage Stats Tests
// ============================================================================

pub async fn test_get_usage_stats_empty(ctx: &UsageTestContext<'_>) {
    let org_id = ctx.create_test_org("test-org").await;
    let api_key_id = ctx.create_test_api_key(org_id, "test-key").await;

    let today = Utc::now().date_naive();
    let range = date_range(today - Duration::days(30), today);
    let stats = ctx
        .usage_repo
        .get_usage_stats(api_key_id, range)
        .await
        .expect("Failed to get usage stats");

    assert_eq!(stats.avg_daily_spend_microcents, 0);
    assert_eq!(stats.std_dev_daily_spend_microcents, 0);
    assert_eq!(stats.sample_days, 0);
}

pub async fn test_get_usage_stats_single_day(ctx: &UsageTestContext<'_>) {
    let org_id = ctx.create_test_org("test-org").await;
    let api_key_id = ctx.create_test_api_key(org_id, "test-key").await;

    let today = Utc::now();

    // Log 3 entries today
    for _ in 0..3 {
        ctx.usage_repo
            .log(create_usage_entry_at_time(api_key_id, "gpt-4", 1000, today))
            .await
            .expect("Failed to log");
    }

    let range = date_range(today.date_naive(), today.date_naive());
    let stats = ctx
        .usage_repo
        .get_usage_stats(api_key_id, range)
        .await
        .expect("Failed to get usage stats");

    assert_eq!(stats.avg_daily_spend_microcents, 3000); // 1000 * 3
    assert_eq!(stats.std_dev_daily_spend_microcents, 0); // Single day = no std dev
    assert_eq!(stats.sample_days, 1);
}

pub async fn test_get_usage_stats_multiple_days(ctx: &UsageTestContext<'_>) {
    let org_id = ctx.create_test_org("test-org").await;
    let api_key_id = ctx.create_test_api_key(org_id, "test-key").await;

    let today = Utc::now();
    let yesterday = today - Duration::days(1);
    let two_days_ago = today - Duration::days(2);

    // Day 1: 1000 microcents
    ctx.usage_repo
        .log(create_usage_entry_at_time(
            api_key_id,
            "gpt-4",
            1000,
            two_days_ago,
        ))
        .await
        .expect("Failed to log");

    // Day 2: 2000 microcents
    ctx.usage_repo
        .log(create_usage_entry_at_time(
            api_key_id, "gpt-4", 2000, yesterday,
        ))
        .await
        .expect("Failed to log");

    // Day 3: 3000 microcents
    ctx.usage_repo
        .log(create_usage_entry_at_time(api_key_id, "gpt-4", 3000, today))
        .await
        .expect("Failed to log");

    let range = date_range(two_days_ago.date_naive(), today.date_naive());
    let stats = ctx
        .usage_repo
        .get_usage_stats(api_key_id, range)
        .await
        .expect("Failed to get usage stats");

    // Average: (1000 + 2000 + 3000) / 3 = 2000
    assert_eq!(stats.avg_daily_spend_microcents, 2000);
    assert_eq!(stats.sample_days, 3);
    // std dev should be 1000 (values are 1000, 2000, 3000 with mean 2000)
    assert!(
        stats.std_dev_daily_spend_microcents >= 900 && stats.std_dev_daily_spend_microcents <= 1100
    );
}

// ============================================================================
// Get Current Period Spend Tests
// ============================================================================

pub async fn test_get_current_period_spend_daily_empty(ctx: &UsageTestContext<'_>) {
    let org_id = ctx.create_test_org("test-org").await;
    let api_key_id = ctx.create_test_api_key(org_id, "test-key").await;

    let spend = ctx
        .usage_repo
        .get_current_period_spend(api_key_id, "daily")
        .await
        .expect("Failed to get current period spend");

    assert_eq!(spend, 0);
}

pub async fn test_get_current_period_spend_daily_with_data(ctx: &UsageTestContext<'_>) {
    let org_id = ctx.create_test_org("test-org").await;
    let api_key_id = ctx.create_test_api_key(org_id, "test-key").await;

    let today = Utc::now();
    let yesterday = today - Duration::days(1);

    // Log entry today
    ctx.usage_repo
        .log(create_usage_entry_at_time(api_key_id, "gpt-4", 1000, today))
        .await
        .expect("Failed to log");

    // Log entry yesterday (should not be counted)
    ctx.usage_repo
        .log(create_usage_entry_at_time(
            api_key_id, "gpt-4", 5000, yesterday,
        ))
        .await
        .expect("Failed to log");

    let spend = ctx
        .usage_repo
        .get_current_period_spend(api_key_id, "daily")
        .await
        .expect("Failed to get current period spend");

    assert_eq!(spend, 1000);
}

pub async fn test_get_current_period_spend_monthly(ctx: &UsageTestContext<'_>) {
    let org_id = ctx.create_test_org("test-org").await;
    let api_key_id = ctx.create_test_api_key(org_id, "test-key").await;

    let today = Utc::now();

    // Log entries on different days this month
    ctx.usage_repo
        .log(create_usage_entry_at_time(api_key_id, "gpt-4", 1000, today))
        .await
        .expect("Failed to log");

    // Log another entry today
    ctx.usage_repo
        .log(create_usage_entry_at_time(api_key_id, "gpt-4", 2000, today))
        .await
        .expect("Failed to log");

    let spend = ctx
        .usage_repo
        .get_current_period_spend(api_key_id, "monthly")
        .await
        .expect("Failed to get current period spend");

    assert_eq!(spend, 3000); // 1000 + 2000
}

pub async fn test_get_current_period_spend_unknown_period(ctx: &UsageTestContext<'_>) {
    let org_id = ctx.create_test_org("test-org").await;
    let api_key_id = ctx.create_test_api_key(org_id, "test-key").await;

    let spend = ctx
        .usage_repo
        .get_current_period_spend(api_key_id, "unknown")
        .await
        .expect("Failed to get current period spend");

    assert_eq!(spend, 0);
}

// ============================================================================
// Aggregated Usage Tests
// ============================================================================

pub async fn test_get_daily_usage_by_org_empty(ctx: &UsageTestContext<'_>) {
    let org_id = ctx.create_test_org("test-org").await;

    let result = ctx
        .usage_repo
        .get_daily_usage_by_org(org_id, today_range())
        .await
        .expect("Failed to get daily usage by org");

    assert!(result.is_empty());
}

pub async fn test_get_daily_usage_by_org_aggregates_multiple_keys(ctx: &UsageTestContext<'_>) {
    let org_id = ctx.create_test_org("test-org").await;
    let key1 = ctx.create_test_api_key(org_id, "key-1").await;
    let key2 = ctx.create_test_api_key(org_id, "key-2").await;

    // Log usage for both keys with org_id set
    let mut e1 = create_usage_entry(key1, "gpt-4", "openai", 100, 50, Some(500));
    e1.org_id = Some(org_id);
    ctx.usage_repo.log(e1).await.expect("Failed to log");

    let mut e2 = create_usage_entry(key2, "gpt-4", "openai", 100, 50, Some(700));
    e2.org_id = Some(org_id);
    ctx.usage_repo.log(e2).await.expect("Failed to log");

    let result = ctx
        .usage_repo
        .get_daily_usage_by_org(org_id, today_range())
        .await
        .expect("Failed to get daily usage by org");

    assert_eq!(result.len(), 1);
    assert_eq!(result[0].total_cost_microcents, 1200); // 500 + 700
    assert_eq!(result[0].request_count, 2);
}

pub async fn test_get_daily_usage_by_org_excludes_other_orgs(ctx: &UsageTestContext<'_>) {
    let org1 = ctx.create_test_org("org-1").await;
    let org2 = ctx.create_test_org("org-2").await;
    let key1 = ctx.create_test_api_key(org1, "key-1").await;
    let key2 = ctx.create_test_api_key(org2, "key-2").await;

    let mut e1 = create_usage_entry(key1, "gpt-4", "openai", 100, 50, Some(500));
    e1.org_id = Some(org1);
    ctx.usage_repo.log(e1).await.expect("Failed to log");

    let mut e2 = create_usage_entry(key2, "gpt-4", "openai", 100, 50, Some(1000));
    e2.org_id = Some(org2);
    ctx.usage_repo.log(e2).await.expect("Failed to log");

    let result = ctx
        .usage_repo
        .get_daily_usage_by_org(org1, today_range())
        .await
        .expect("Failed to get daily usage by org");

    assert_eq!(result.len(), 1);
    assert_eq!(result[0].total_cost_microcents, 500);
}

pub async fn test_get_summary_by_org(ctx: &UsageTestContext<'_>) {
    let org_id = ctx.create_test_org("test-org").await;
    let key1 = ctx.create_test_api_key(org_id, "key-1").await;
    let key2 = ctx.create_test_api_key(org_id, "key-2").await;

    let mut e1 = create_usage_entry(key1, "gpt-4", "openai", 100, 50, Some(500));
    e1.org_id = Some(org_id);
    ctx.usage_repo.log(e1).await.expect("Failed to log");

    let mut e2 = create_usage_entry(key2, "gpt-4", "openai", 200, 100, Some(700));
    e2.org_id = Some(org_id);
    ctx.usage_repo.log(e2).await.expect("Failed to log");

    let summary = ctx
        .usage_repo
        .get_summary_by_org(org_id, today_range())
        .await
        .expect("Failed to get summary by org");

    assert_eq!(summary.total_cost_microcents, 1200);
    assert_eq!(summary.total_tokens, 450); // (100+50) + (200+100)
    assert_eq!(summary.request_count, 2);
}

pub async fn test_get_model_usage_by_org(ctx: &UsageTestContext<'_>) {
    let org_id = ctx.create_test_org("test-org").await;
    let key1 = ctx.create_test_api_key(org_id, "key-1").await;
    let key2 = ctx.create_test_api_key(org_id, "key-2").await;

    // Different models across multiple keys
    let mut e1 = create_usage_entry(key1, "gpt-4", "openai", 100, 50, Some(500));
    e1.org_id = Some(org_id);
    ctx.usage_repo.log(e1).await.expect("Failed to log");

    let mut e2 = create_usage_entry(key2, "gpt-4", "openai", 100, 50, Some(500));
    e2.org_id = Some(org_id);
    ctx.usage_repo.log(e2).await.expect("Failed to log");

    let mut e3 = create_usage_entry(key1, "claude-3-opus", "anthropic", 100, 50, Some(1000));
    e3.org_id = Some(org_id);
    ctx.usage_repo.log(e3).await.expect("Failed to log");

    let result = ctx
        .usage_repo
        .get_model_usage_by_org(org_id, today_range())
        .await
        .expect("Failed to get model usage by org");

    assert_eq!(result.len(), 2);

    let gpt4 = result.iter().find(|m| m.model == "gpt-4").unwrap();
    let claude = result.iter().find(|m| m.model == "claude-3-opus").unwrap();

    assert_eq!(gpt4.total_cost_microcents, 1000);
    assert_eq!(gpt4.request_count, 2);
    assert_eq!(claude.total_cost_microcents, 1000);
    assert_eq!(claude.request_count, 1);
}

pub async fn test_get_provider_usage_by_org(ctx: &UsageTestContext<'_>) {
    let org_id = ctx.create_test_org("test-org").await;
    let key1 = ctx.create_test_api_key(org_id, "key-1").await;

    let mut e1 = create_usage_entry(key1, "gpt-4", "openai", 100, 50, Some(500));
    e1.org_id = Some(org_id);
    ctx.usage_repo.log(e1).await.expect("Failed to log");

    let mut e2 = create_usage_entry(key1, "claude-3-opus", "anthropic", 100, 50, Some(1000));
    e2.org_id = Some(org_id);
    ctx.usage_repo.log(e2).await.expect("Failed to log");

    let result = ctx
        .usage_repo
        .get_provider_usage_by_org(org_id, today_range())
        .await
        .expect("Failed to get provider usage by org");

    assert_eq!(result.len(), 2);

    let openai = result.iter().find(|p| p.provider == "openai").unwrap();
    let anthropic = result.iter().find(|p| p.provider == "anthropic").unwrap();

    assert_eq!(openai.total_cost_microcents, 500);
    assert_eq!(anthropic.total_cost_microcents, 1000);
}

pub async fn test_get_usage_stats_by_org(ctx: &UsageTestContext<'_>) {
    let org_id = ctx.create_test_org("test-org").await;
    let key1 = ctx.create_test_api_key(org_id, "key-1").await;
    let key2 = ctx.create_test_api_key(org_id, "key-2").await;

    let today = Utc::now();
    let yesterday = today - Duration::days(1);

    // Day 1: 2000 microcents total (1000 + 1000)
    let mut e1 = create_usage_entry_at_time(key1, "gpt-4", 1000, yesterday);
    e1.org_id = Some(org_id);
    ctx.usage_repo.log(e1).await.expect("Failed to log");

    let mut e2 = create_usage_entry_at_time(key2, "gpt-4", 1000, yesterday);
    e2.org_id = Some(org_id);
    ctx.usage_repo.log(e2).await.expect("Failed to log");

    // Day 2: 4000 microcents total (2000 + 2000)
    let mut e3 = create_usage_entry_at_time(key1, "gpt-4", 2000, today);
    e3.org_id = Some(org_id);
    ctx.usage_repo.log(e3).await.expect("Failed to log");

    let mut e4 = create_usage_entry_at_time(key2, "gpt-4", 2000, today);
    e4.org_id = Some(org_id);
    ctx.usage_repo.log(e4).await.expect("Failed to log");

    let range = date_range(yesterday.date_naive(), today.date_naive());
    let stats = ctx
        .usage_repo
        .get_usage_stats_by_org(org_id, range)
        .await
        .expect("Failed to get usage stats by org");

    // Average: (2000 + 4000) / 2 = 3000
    assert_eq!(stats.avg_daily_spend_microcents, 3000);
    assert_eq!(stats.sample_days, 2);
}

pub async fn test_get_daily_usage_by_provider(ctx: &UsageTestContext<'_>) {
    let org_id = ctx.create_test_org("test-org").await;
    let key1 = ctx.create_test_api_key(org_id, "key-1").await;

    let today = Utc::now();
    let yesterday = today - Duration::days(1);

    // OpenAI usage across multiple days
    ctx.usage_repo
        .log(create_usage_entry_at_time(key1, "gpt-4", 500, yesterday))
        .await
        .expect("Failed to log");
    ctx.usage_repo
        .log(create_usage_entry_at_time(key1, "gpt-4", 1000, today))
        .await
        .expect("Failed to log");

    let range = date_range(yesterday.date_naive(), today.date_naive());
    let result = ctx
        .usage_repo
        .get_daily_usage_by_provider("openai", range)
        .await
        .expect("Failed to get daily usage by provider");

    assert_eq!(result.len(), 2);

    let yesterday_spend = result
        .iter()
        .find(|d| d.date == yesterday.date_naive())
        .unwrap();
    let today_spend = result
        .iter()
        .find(|d| d.date == today.date_naive())
        .unwrap();

    assert_eq!(yesterday_spend.total_cost_microcents, 500);
    assert_eq!(today_spend.total_cost_microcents, 1000);
}

// ============================================================================
// Session User (No API Key) Tests
// ============================================================================

pub async fn test_log_session_user_no_api_key(ctx: &UsageTestContext<'_>) {
    let org_id = ctx.create_test_org("session-org").await;
    let user_id = Uuid::new_v4();

    // Session user: no API key, just user_id + org_id
    let entry = create_attributed_usage_entry(
        UsageAttribution {
            user_id: Some(user_id),
            org_id: Some(org_id),
            ..Default::default()
        },
        "gpt-4",
        "openai",
        1000,
    );
    ctx.usage_repo
        .log(entry)
        .await
        .expect("Failed to log session usage");

    // Should appear in org aggregation
    let summary = ctx
        .usage_repo
        .get_summary_by_org(org_id, today_range())
        .await
        .unwrap();
    assert_eq!(summary.request_count, 1);
    assert_eq!(summary.total_cost_microcents, 1000);

    // Should appear in user aggregation
    let user_summary = ctx
        .usage_repo
        .get_summary_by_user(user_id, today_range())
        .await
        .unwrap();
    assert_eq!(user_summary.request_count, 1);
    assert_eq!(user_summary.total_cost_microcents, 1000);
}

pub async fn test_log_session_user_with_project(ctx: &UsageTestContext<'_>) {
    let org_id = ctx.create_test_org("session-proj-org").await;
    let user_id = Uuid::new_v4();
    let project_id = Uuid::new_v4();

    // Session user with project context from X-Hadrian-Project header
    let entry = create_attributed_usage_entry(
        UsageAttribution {
            user_id: Some(user_id),
            org_id: Some(org_id),
            project_id: Some(project_id),
            ..Default::default()
        },
        "gpt-4",
        "openai",
        2000,
    );
    ctx.usage_repo.log(entry).await.expect("Failed to log");

    // Should appear in project aggregation
    let summary = ctx
        .usage_repo
        .get_summary_by_project(project_id, today_range())
        .await
        .unwrap();
    assert_eq!(summary.request_count, 1);
    assert_eq!(summary.total_cost_microcents, 2000);
}

pub async fn test_org_aggregation_includes_session_and_api_key_usage(ctx: &UsageTestContext<'_>) {
    let org_id = ctx.create_test_org("mixed-org").await;
    let api_key_id = ctx.create_test_api_key(org_id, "api-key").await;
    let user_id = Uuid::new_v4();

    // API key usage with org_id
    let mut api_entry = create_usage_entry(api_key_id, "gpt-4", "openai", 100, 50, Some(500));
    api_entry.org_id = Some(org_id);
    ctx.usage_repo.log(api_entry).await.unwrap();

    // Session user usage with same org_id
    let session_entry = create_attributed_usage_entry(
        UsageAttribution {
            user_id: Some(user_id),
            org_id: Some(org_id),
            ..Default::default()
        },
        "gpt-4",
        "openai",
        700,
    );
    ctx.usage_repo.log(session_entry).await.unwrap();

    // Org aggregation should include both
    let summary = ctx
        .usage_repo
        .get_summary_by_org(org_id, today_range())
        .await
        .unwrap();
    assert_eq!(summary.request_count, 2);
    assert_eq!(summary.total_cost_microcents, 1200);
}

// ============================================================================
// Team-Level Aggregation Tests
// ============================================================================

pub async fn test_get_summary_by_team(ctx: &UsageTestContext<'_>) {
    let org_id = ctx.create_test_org("team-org").await;
    let team_id = Uuid::new_v4();
    let api_key_id = ctx.create_test_api_key(org_id, "team-key").await;

    // Team-scoped API key usage
    let entry = create_attributed_usage_entry(
        UsageAttribution {
            api_key_id: Some(api_key_id),
            org_id: Some(org_id),
            team_id: Some(team_id),
            ..Default::default()
        },
        "gpt-4",
        "openai",
        1500,
    );
    ctx.usage_repo.log(entry).await.unwrap();

    let summary = ctx
        .usage_repo
        .get_summary_by_team(team_id, today_range())
        .await
        .unwrap();
    assert_eq!(summary.request_count, 1);
    assert_eq!(summary.total_cost_microcents, 1500);
}

pub async fn test_get_daily_usage_by_team(ctx: &UsageTestContext<'_>) {
    let org_id = ctx.create_test_org("team-daily-org").await;
    let team_id = Uuid::new_v4();

    let today = Utc::now();
    let yesterday = today - Duration::days(1);

    let mut entry1 = create_attributed_usage_entry(
        UsageAttribution {
            org_id: Some(org_id),
            team_id: Some(team_id),
            ..Default::default()
        },
        "gpt-4",
        "openai",
        500,
    );
    entry1.request_at = yesterday;
    ctx.usage_repo.log(entry1).await.unwrap();

    let entry2 = create_attributed_usage_entry(
        UsageAttribution {
            org_id: Some(org_id),
            team_id: Some(team_id),
            ..Default::default()
        },
        "gpt-4",
        "openai",
        1000,
    );
    ctx.usage_repo.log(entry2).await.unwrap();

    let range = date_range(yesterday.date_naive(), today.date_naive());
    let result = ctx
        .usage_repo
        .get_daily_usage_by_team(team_id, range)
        .await
        .unwrap();

    assert_eq!(result.len(), 2);
}

pub async fn test_get_model_usage_by_team(ctx: &UsageTestContext<'_>) {
    let org_id = ctx.create_test_org("team-model-org").await;
    let team_id = Uuid::new_v4();

    let team_attr = || UsageAttribution {
        org_id: Some(org_id),
        team_id: Some(team_id),
        ..Default::default()
    };
    ctx.usage_repo
        .log(create_attributed_usage_entry(
            team_attr(),
            "gpt-4",
            "openai",
            500,
        ))
        .await
        .unwrap();
    ctx.usage_repo
        .log(create_attributed_usage_entry(
            team_attr(),
            "claude-3-opus",
            "anthropic",
            1000,
        ))
        .await
        .unwrap();

    let result = ctx
        .usage_repo
        .get_model_usage_by_team(team_id, today_range())
        .await
        .unwrap();
    assert_eq!(result.len(), 2);

    let claude = result.iter().find(|m| m.model == "claude-3-opus").unwrap();
    assert_eq!(claude.total_cost_microcents, 1000);
}

pub async fn test_get_provider_usage_by_team(ctx: &UsageTestContext<'_>) {
    let org_id = ctx.create_test_org("team-provider-org").await;
    let team_id = Uuid::new_v4();

    let team_attr = || UsageAttribution {
        org_id: Some(org_id),
        team_id: Some(team_id),
        ..Default::default()
    };
    ctx.usage_repo
        .log(create_attributed_usage_entry(
            team_attr(),
            "gpt-4",
            "openai",
            500,
        ))
        .await
        .unwrap();
    ctx.usage_repo
        .log(create_attributed_usage_entry(
            team_attr(),
            "claude-3-opus",
            "anthropic",
            1000,
        ))
        .await
        .unwrap();

    let result = ctx
        .usage_repo
        .get_provider_usage_by_team(team_id, today_range())
        .await
        .unwrap();
    assert_eq!(result.len(), 2);

    let anthropic = result.iter().find(|p| p.provider == "anthropic").unwrap();
    assert_eq!(anthropic.total_cost_microcents, 1000);
}

pub async fn test_team_aggregation_excludes_other_teams(ctx: &UsageTestContext<'_>) {
    let org_id = ctx.create_test_org("team-excl-org").await;
    let team_a = Uuid::new_v4();
    let team_b = Uuid::new_v4();

    ctx.usage_repo
        .log(create_attributed_usage_entry(
            UsageAttribution {
                org_id: Some(org_id),
                team_id: Some(team_a),
                ..Default::default()
            },
            "gpt-4",
            "openai",
            500,
        ))
        .await
        .unwrap();
    ctx.usage_repo
        .log(create_attributed_usage_entry(
            UsageAttribution {
                org_id: Some(org_id),
                team_id: Some(team_b),
                ..Default::default()
            },
            "gpt-4",
            "openai",
            1000,
        ))
        .await
        .unwrap();

    let summary_a = ctx
        .usage_repo
        .get_summary_by_team(team_a, today_range())
        .await
        .unwrap();
    let summary_b = ctx
        .usage_repo
        .get_summary_by_team(team_b, today_range())
        .await
        .unwrap();

    assert_eq!(summary_a.total_cost_microcents, 500);
    assert_eq!(summary_b.total_cost_microcents, 1000);
}

// ============================================================================
// Record Type Filter Tests
// ============================================================================

pub async fn test_get_summary_record_type_filter(ctx: &UsageTestContext<'_>) {
    let org_id = ctx.create_test_org("rt-org").await;
    let api_key_id = ctx.create_test_api_key(org_id, "rt-key").await;

    ctx.usage_repo
        .log(create_usage_entry(
            api_key_id,
            "gpt-4",
            "openai",
            100,
            50,
            Some(500),
        ))
        .await
        .unwrap();
    ctx.usage_repo
        .log(create_tool_usage_entry(api_key_id, 250))
        .await
        .unwrap();

    // No filter includes both record types
    let all = ctx
        .usage_repo
        .get_summary(api_key_id, today_range())
        .await
        .unwrap();
    assert_eq!(all.request_count, 2);
    assert_eq!(all.total_cost_microcents, 750);

    // "model" excludes the tool row
    let models = ctx
        .usage_repo
        .get_summary(api_key_id, today_range_of("model"))
        .await
        .unwrap();
    assert_eq!(models.request_count, 1);
    assert_eq!(models.total_cost_microcents, 500);

    // "tool" excludes the model row
    let tools = ctx
        .usage_repo
        .get_summary(api_key_id, today_range_of("tool"))
        .await
        .unwrap();
    assert_eq!(tools.request_count, 1);
    assert_eq!(tools.total_cost_microcents, 250);
}

pub async fn test_get_by_model_record_type_filter(ctx: &UsageTestContext<'_>) {
    let org_id = ctx.create_test_org("rt-model-org").await;
    let api_key_id = ctx.create_test_api_key(org_id, "rt-model-key").await;

    ctx.usage_repo
        .log(create_usage_entry(
            api_key_id,
            "gpt-4",
            "openai",
            100,
            50,
            Some(500),
        ))
        .await
        .unwrap();
    ctx.usage_repo
        .log(create_tool_usage_entry(api_key_id, 250))
        .await
        .unwrap();

    // Grouped aggregation honors the filter too
    let models = ctx
        .usage_repo
        .get_by_model(api_key_id, today_range_of("model"))
        .await
        .unwrap();
    assert_eq!(models.len(), 1);
    assert_eq!(models[0].model, "gpt-4");
    assert_eq!(models[0].total_cost_microcents, 500);

    let all = ctx
        .usage_repo
        .get_by_model(api_key_id, today_range())
        .await
        .unwrap();
    assert_eq!(all.len(), 2);
}

// ============================================================================
// SQLite Tests - Fast, in-memory
// ============================================================================

#[cfg(all(test, feature = "database-sqlite"))]
mod sqlite_tests {
    use super::*;
    use crate::db::{
        sqlite::{SqliteApiKeyRepo, SqliteOrganizationRepo, SqliteUsageRepo},
        tests::harness::{create_sqlite_pool, run_sqlite_migrations},
    };

    async fn create_repos() -> (SqliteUsageRepo, SqliteApiKeyRepo, SqliteOrganizationRepo) {
        let pool = create_sqlite_pool().await;
        run_sqlite_migrations(&pool).await;
        (
            SqliteUsageRepo::new(pool.clone()),
            SqliteApiKeyRepo::new(pool.clone()),
            SqliteOrganizationRepo::new(pool),
        )
    }

    macro_rules! sqlite_test {
        ($name:ident) => {
            #[tokio::test]
            async fn $name() {
                let (usage_repo, api_key_repo, org_repo) = create_repos().await;
                let ctx = UsageTestContext {
                    usage_repo: &usage_repo,
                    api_key_repo: &api_key_repo,
                    org_repo: &org_repo,
                };
                super::$name(&ctx).await;
            }
        };
    }

    // Log tests
    sqlite_test!(test_log_basic);
    sqlite_test!(test_log_calculates_total_tokens);
    sqlite_test!(test_log_with_no_cost);
    sqlite_test!(test_log_with_referer);
    sqlite_test!(test_log_multiple_entries);
    sqlite_test!(test_log_tool_runtime_seconds_round_trip);

    // Log batch tests
    sqlite_test!(test_log_batch_empty);
    sqlite_test!(test_log_batch_basic);
    sqlite_test!(test_log_batch_large_batch_spans_multiple_chunks);
    sqlite_test!(test_log_batch_ignores_duplicates);
    sqlite_test!(test_log_batch_mixed_new_and_duplicate);

    // Summary tests
    sqlite_test!(test_get_summary_empty);
    sqlite_test!(test_get_summary_with_records);
    sqlite_test!(test_get_summary_filters_by_api_key);
    sqlite_test!(test_get_summary_filters_by_date_range);
    sqlite_test!(test_get_summary_record_type_filter);

    // Get by date tests
    sqlite_test!(test_get_by_date_empty);
    sqlite_test!(test_get_by_date_groups_by_date);
    sqlite_test!(test_get_by_date_filters_by_api_key);

    // Get by model tests
    sqlite_test!(test_get_by_model_empty);
    sqlite_test!(test_get_by_model_groups_by_model);
    sqlite_test!(test_get_by_model_ordered_by_cost_desc);
    sqlite_test!(test_get_by_model_filters_by_date_range);
    sqlite_test!(test_get_by_model_record_type_filter);

    // Get by referer tests
    sqlite_test!(test_get_by_referer_empty);
    sqlite_test!(test_get_by_referer_groups_by_referer);
    sqlite_test!(test_get_by_referer_handles_null_referer);
    sqlite_test!(test_get_by_referer_ordered_by_cost_desc);
    sqlite_test!(test_get_by_referer_filters_by_api_key);

    // Get usage stats tests
    sqlite_test!(test_get_usage_stats_empty);
    sqlite_test!(test_get_usage_stats_single_day);
    sqlite_test!(test_get_usage_stats_multiple_days);

    // Get current period spend tests
    sqlite_test!(test_get_current_period_spend_daily_empty);
    sqlite_test!(test_get_current_period_spend_daily_with_data);
    sqlite_test!(test_get_current_period_spend_monthly);
    sqlite_test!(test_get_current_period_spend_unknown_period);

    // Aggregated usage tests
    sqlite_test!(test_get_daily_usage_by_org_empty);
    sqlite_test!(test_get_daily_usage_by_org_aggregates_multiple_keys);
    sqlite_test!(test_get_daily_usage_by_org_excludes_other_orgs);
    sqlite_test!(test_get_summary_by_org);
    sqlite_test!(test_get_model_usage_by_org);
    sqlite_test!(test_get_provider_usage_by_org);
    sqlite_test!(test_get_usage_stats_by_org);
    sqlite_test!(test_get_daily_usage_by_provider);

    // Session user tests
    sqlite_test!(test_log_session_user_no_api_key);
    sqlite_test!(test_log_session_user_with_project);
    sqlite_test!(test_org_aggregation_includes_session_and_api_key_usage);

    // Team-level aggregation tests
    sqlite_test!(test_get_summary_by_team);
    sqlite_test!(test_get_daily_usage_by_team);
    sqlite_test!(test_get_model_usage_by_team);
    sqlite_test!(test_get_provider_usage_by_team);
    sqlite_test!(test_team_aggregation_excludes_other_teams);
}

// ============================================================================
// PostgreSQL Tests - Require Docker, run with `cargo test -- --ignored`
// ============================================================================

#[cfg(all(test, feature = "database-postgres"))]
mod postgres_tests {
    use super::UsageTestContext;
    use crate::db::{
        postgres::{PostgresApiKeyRepo, PostgresOrganizationRepo, PostgresUsageRepo},
        tests::harness::postgres::{create_isolated_postgres_pool, run_postgres_migrations},
    };

    macro_rules! postgres_test {
        ($name:ident) => {
            #[tokio::test]
            #[ignore = "Requires Docker - run with `cargo test -- --ignored`"]
            async fn $name() {
                let pool = create_isolated_postgres_pool().await;
                run_postgres_migrations(&pool).await;
                let usage_repo = PostgresUsageRepo::new(pool.clone(), None);
                let api_key_repo = PostgresApiKeyRepo::new(pool.clone(), None);
                let org_repo = PostgresOrganizationRepo::new(pool, None);
                let ctx = UsageTestContext {
                    usage_repo: &usage_repo,
                    api_key_repo: &api_key_repo,
                    org_repo: &org_repo,
                };
                super::$name(&ctx).await;
            }
        };
    }

    // Log tests
    postgres_test!(test_log_basic);
    postgres_test!(test_log_calculates_total_tokens);
    postgres_test!(test_log_with_no_cost);
    postgres_test!(test_log_with_referer);
    postgres_test!(test_log_multiple_entries);
    postgres_test!(test_log_tool_runtime_seconds_round_trip);

    // Log batch tests
    postgres_test!(test_log_batch_empty);
    postgres_test!(test_log_batch_basic);
    postgres_test!(test_log_batch_large_batch_spans_multiple_chunks);
    postgres_test!(test_log_batch_ignores_duplicates);
    postgres_test!(test_log_batch_mixed_new_and_duplicate);

    // Summary tests
    postgres_test!(test_get_summary_empty);
    postgres_test!(test_get_summary_with_records);
    postgres_test!(test_get_summary_filters_by_api_key);
    postgres_test!(test_get_summary_filters_by_date_range);
    postgres_test!(test_get_summary_record_type_filter);

    // Get by date tests
    postgres_test!(test_get_by_date_empty);
    postgres_test!(test_get_by_date_groups_by_date);
    postgres_test!(test_get_by_date_filters_by_api_key);

    // Get by model tests
    postgres_test!(test_get_by_model_empty);
    postgres_test!(test_get_by_model_groups_by_model);
    postgres_test!(test_get_by_model_ordered_by_cost_desc);
    postgres_test!(test_get_by_model_filters_by_date_range);
    postgres_test!(test_get_by_model_record_type_filter);

    // Get by referer tests
    postgres_test!(test_get_by_referer_empty);
    postgres_test!(test_get_by_referer_groups_by_referer);
    postgres_test!(test_get_by_referer_handles_null_referer);
    postgres_test!(test_get_by_referer_ordered_by_cost_desc);
    postgres_test!(test_get_by_referer_filters_by_api_key);

    // Get usage stats tests
    postgres_test!(test_get_usage_stats_empty);
    postgres_test!(test_get_usage_stats_single_day);
    postgres_test!(test_get_usage_stats_multiple_days);

    // Get current period spend tests
    postgres_test!(test_get_current_period_spend_daily_empty);
    postgres_test!(test_get_current_period_spend_daily_with_data);
    postgres_test!(test_get_current_period_spend_monthly);
    postgres_test!(test_get_current_period_spend_unknown_period);

    // Aggregated usage tests
    postgres_test!(test_get_daily_usage_by_org_empty);
    postgres_test!(test_get_daily_usage_by_org_aggregates_multiple_keys);
    postgres_test!(test_get_daily_usage_by_org_excludes_other_orgs);
    postgres_test!(test_get_summary_by_org);
    postgres_test!(test_get_model_usage_by_org);
    postgres_test!(test_get_provider_usage_by_org);
    postgres_test!(test_get_usage_stats_by_org);
    postgres_test!(test_get_daily_usage_by_provider);

    // Session user tests
    postgres_test!(test_log_session_user_no_api_key);
    postgres_test!(test_log_session_user_with_project);
    postgres_test!(test_org_aggregation_includes_session_and_api_key_usage);

    // Team-level aggregation tests
    postgres_test!(test_get_summary_by_team);
    postgres_test!(test_get_daily_usage_by_team);
    postgres_test!(test_get_model_usage_by_team);
    postgres_test!(test_get_provider_usage_by_team);
    postgres_test!(test_team_aggregation_excludes_other_teams);
}
